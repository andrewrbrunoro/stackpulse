//! Versioned, portable JSONL conversations. SQLite is read only during migration.
use crate::{activity::ActivitySnapshot, workflow::Execution};
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

#[path = "chat_history_import.rs"]
mod import;
#[path = "chat_history_migration.rs"]
mod migration;

const EMPTY_TITLE: &str = "Nova conversa";
const INTERRUPTED: &str = "Interrompido · execução encerrada antes da conclusão";
const FORMAT: &str = "stackpulse.session";
const VERSION: u32 = 1;

pub(crate) struct ChatHistory {
    project: PathBuf,
    root: PathBuf,
    project_id: String,
    owned: RefCell<HashMap<String, (File, Document)>>,
    warnings: RefCell<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SessionSummary {
    pub id: String,
    pub title: String,
    pub custom_title: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub turns: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredSession {
    pub summary: SessionSummary,
    pub turns: Vec<StoredTurn>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredTurn {
    pub id: String,
    pub prompt: String,
    pub response: Vec<String>,
    pub status: String,
    pub profile: String,
    pub execution: Option<Execution>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    /// Native telemetry is preserved without interpreting missing values as zero.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_events: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AgentMessage {
    pub id: String,
    pub agent_id: String,
    pub redirect: bool,
    pub message: String,
    pub status: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Document {
    session: StoredSession,
    #[serde(default)]
    activities: HashMap<String, ActivitySnapshot>,
    #[serde(default)]
    messages: HashMap<String, Vec<AgentMessage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<Origin>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Origin {
    client: String,
    session_id: String,
}

#[derive(Serialize, Deserialize)]
struct Record {
    format: String,
    version: u32,
    project_id: String,
    session_id: String,
    #[serde(flatten)]
    event: Event,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    Snapshot {
        document: Box<Document>,
    },
    Renamed {
        title: String,
    },
    Started {
        turn: Box<StoredTurn>,
        at: DateTime<Utc>,
    },
    Updated {
        turn_id: String,
        keep_lines: usize,
        append_lines: Vec<String>,
        status: String,
        execution: Option<Box<Execution>>,
        ended_at: Option<DateTime<Utc>>,
        at: DateTime<Utc>,
    },
    Activity {
        turn_id: String,
        snapshot: ActivitySnapshot,
    },
    AgentMessage {
        turn_id: String,
        message: AgentMessage,
    },
    Finished {
        at: DateTime<Utc>,
    },
    Resumed {
        at: DateTime<Utc>,
    },
}

#[derive(Serialize, Deserialize)]
struct Project {
    format: String,
    version: u32,
    id: String,
}

impl ChatHistory {
    pub(crate) fn path_for(project: &Path) -> PathBuf {
        project.join(".stackpulse/sessions")
    }

    pub(crate) fn open(project: &Path, metrics: &Path) -> Result<Self> {
        let project = project
            .canonicalize()
            .context("Pasta do projeto indisponível")?;
        let root = Self::path_for(&project);
        fs::create_dir_all(root.join(".locks"))?;
        let parent = root.parent().unwrap();
        let project_file = parent.join("project.json");
        if !project_file.exists() {
            let identity = Project {
                format: "stackpulse.project".into(),
                version: VERSION,
                id: uuid::Uuid::new_v4().to_string(),
            };
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            serde_json::to_writer_pretty(&mut temp, &identity)?;
            temp.as_file().sync_all()?;
            if let Err(error) = temp.persist_noclobber(&project_file)
                && error.error.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(error.error.into());
            }
        }
        let identity: Project = serde_json::from_reader(File::open(&project_file)?)?;
        ensure!(
            identity.format == "stackpulse.project" && identity.version == VERSION,
            "Versão de projeto StackPulse não suportada"
        );
        // The local transcript should not accidentally become repository content.
        if let Ok(mut ignore) = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(parent.join(".gitignore"))
        {
            ignore.write_all(b"*\n")?;
        }
        let history = Self {
            project,
            root,
            project_id: identity.id,
            owned: RefCell::new(HashMap::new()),
            warnings: RefCell::new(Vec::new()),
        };
        let count = history.migrate(metrics)?;
        if count > 0 {
            history.warnings.borrow_mut().push(format!(
                "{count} {} para JSONL; banco anterior preservado.",
                if count == 1 {
                    "conversa migrada"
                } else {
                    "conversas migradas"
                }
            ));
        }
        Ok(history)
    }

    pub(crate) fn restore_execution_cache(&self, db: &mut crate::db::Db) -> Result<()> {
        let mut known: std::collections::HashSet<_> =
            db.executions()?.into_iter().map(|job| job.id).collect();
        for summary in self.sessions(&self.project)? {
            for turn in self.read_session(&summary.id)?.turns {
                if let Some(job) = turn.execution
                    && known.insert(job.id.clone())
                {
                    db.save_execution(&job)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn take_warnings(&self) -> String {
        self.warnings.take().join(" · ")
    }

    fn check_project(&self, project: &Path) -> Result<()> {
        ensure!(
            project.canonicalize().ok().as_ref() == Some(&self.project),
            "Sessão de conversa não encontrada neste projeto"
        );
        Ok(())
    }

    fn path(&self, id: &str) -> Result<PathBuf> {
        ensure!(valid_id(id), "Identificador de conversa inválido");
        Ok(self.root.join(format!("{id}.jsonl")))
    }

    fn lock(&self, id: &str) -> Result<File> {
        self.path(id)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join(".locks").join(format!("{id}.lock")))?;
        file.try_lock()
            .map_err(|_| anyhow::anyhow!("Essa sessão está aberta em outro terminal"))?;
        Ok(file)
    }

    fn write_new(&self, document: &Document) -> Result<()> {
        let id = &document.session.summary.id;
        let mut temp = tempfile::NamedTempFile::new_in(&self.root)?;
        self.write_record(
            &mut temp,
            id,
            Event::Snapshot {
                document: Box::new(document.clone()),
            },
        )?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(self.path(id)?)
            .map_err(|error| error.error)?;
        Ok(())
    }

    fn write_record(&self, out: &mut impl Write, id: &str, event: Event) -> Result<()> {
        let record = Record {
            format: FORMAT.into(),
            version: VERSION,
            project_id: self.project_id.clone(),
            session_id: id.into(),
            event,
        };
        serde_json::to_writer(&mut *out, &record)?;
        out.write_all(b"\n")?;
        Ok(())
    }

    fn append(&self, id: &str, event: Event) -> Result<()> {
        let mut owned = self.owned.borrow_mut();
        let (_, current) = owned
            .get_mut(id)
            .context("Conversa não encontrada ou aberta em outro terminal.")?;
        let mut next = current.clone();
        apply(&mut next, &event)?;
        let mut bytes = Vec::new();
        self.write_record(&mut bytes, id, event)?;
        let mut file = OpenOptions::new().append(true).open(self.path(id)?)?;
        let original_len = file.metadata()?.len();
        if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_data()) {
            // Keep this writer usable after a failed partial append.
            let _ = file.set_len(original_len);
            return Err(error.into());
        }
        *current = next;
        Ok(())
    }

    fn document(&self, id: &str) -> Result<Document> {
        if let Some((_, doc)) = self.owned.borrow().get(id) {
            return Ok(doc.clone());
        }
        let (document, _) = read_document(&self.path(id)?)?;
        ensure!(
            document.session.summary.id == id,
            "ID divergente no arquivo da conversa"
        );
        Ok(document)
    }

    pub(crate) fn create_session(&mut self, project: &Path) -> Result<SessionSummary> {
        self.check_project(project)?;
        let now = Utc::now();
        let summary = SessionSummary {
            id: uuid::Uuid::new_v4().to_string(),
            title: EMPTY_TITLE.into(),
            custom_title: false,
            created_at: now,
            updated_at: now,
            ended_at: None,
            turns: 0,
        };
        let lock = self.lock(&summary.id)?;
        let document = Document {
            session: StoredSession {
                summary: summary.clone(),
                turns: vec![],
            },
            activities: HashMap::new(),
            messages: HashMap::new(),
            origin: None,
        };
        self.write_new(&document)?;
        self.owned
            .borrow_mut()
            .insert(summary.id.clone(), (lock, document));
        Ok(summary)
    }

    pub(crate) fn rename_session(&self, id: &str, project: &Path, value: &str) -> Result<String> {
        self.check_project(project)?;
        let title = crate::chat_agents::sanitize(value);
        ensure!(!title.is_empty(), "Escreva um título para a conversa.");
        ensure!(
            title.chars().count() <= 80,
            "Use até 80 caracteres no título."
        );
        self.append(
            id,
            Event::Renamed {
                title: title.clone(),
            },
        )?;
        Ok(title)
    }

    pub(crate) fn sessions(&self, project: &Path) -> Result<Vec<SessionSummary>> {
        self.check_project(project)?;
        let mut summaries = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|id| id.to_str()) else {
                continue;
            };
            match self.document(id) {
                Ok(doc) => {
                    let summary = doc.session.summary;
                    if summary.turns > 0
                        || summary.custom_title
                        || self.owned.borrow().contains_key(id)
                    {
                        summaries.push(summary);
                    }
                }
                Err(error) => self
                    .warnings
                    .borrow_mut()
                    .push(format!("{}: {error:#}", path.display())),
            }
        }
        summaries.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then(b.created_at.cmp(&a.created_at))
                .then(b.id.cmp(&a.id))
        });
        Ok(summaries)
    }

    pub(crate) fn start_turn(
        &mut self,
        session: &str,
        prompt: &str,
        profile: &str,
        status: &str,
    ) -> Result<String> {
        let now = Utc::now();
        let turn = StoredTurn {
            id: uuid::Uuid::new_v4().to_string(),
            prompt: prompt.into(),
            response: vec![],
            status: status.into(),
            profile: profile.into(),
            execution: None,
            started_at: Some(now),
            ended_at: None,
            source_events: vec![],
        };
        let id = turn.id.clone();
        self.append(
            session,
            Event::Started {
                turn: Box::new(turn),
                at: now,
            },
        )?;
        Ok(id)
    }

    pub(crate) fn update_turn(
        &mut self,
        session: &str,
        turn_id: &str,
        response: &[String],
        status: &str,
        execution: Option<&Execution>,
        finished: bool,
    ) -> Result<()> {
        let doc = self.document(session)?;
        let turn = doc
            .session
            .turns
            .iter()
            .find(|turn| turn.id == turn_id)
            .context("Turno de conversa não encontrado")?;
        let ended_at = turn.ended_at.or_else(|| finished.then(Utc::now));
        if turn.response == response
            && turn.status == status
            && turn.ended_at == ended_at
            && serde_json::to_value(&turn.execution)? == serde_json::to_value(execution)?
        {
            return Ok(());
        }
        let keep_lines = turn
            .response
            .iter()
            .zip(response)
            .take_while(|(a, b)| a == b)
            .count();
        self.append(
            session,
            Event::Updated {
                turn_id: turn_id.into(),
                keep_lines,
                append_lines: response[keep_lines..].to_vec(),
                status: status.into(),
                execution: execution.cloned().map(Box::new),
                ended_at,
                at: Utc::now(),
            },
        )
    }

    pub(crate) fn save_activity(
        &self,
        session: &str,
        turn: &str,
        snapshot: &ActivitySnapshot,
    ) -> Result<()> {
        let doc = self.document(session)?;
        ensure!(
            doc.session.turns.iter().any(|t| t.id == turn),
            "Pedido não encontrado nesta conversa"
        );
        if doc
            .activities
            .get(turn)
            .map(serde_json::to_value)
            .transpose()?
            == Some(serde_json::to_value(snapshot)?)
        {
            return Ok(());
        }
        self.append(
            session,
            Event::Activity {
                turn_id: turn.into(),
                snapshot: snapshot.clone(),
            },
        )
    }

    fn session_for_turn(&self, turn: &str) -> Result<String> {
        for (id, (_, doc)) in self.owned.borrow().iter() {
            if doc.session.turns.iter().any(|t| t.id == turn) {
                return Ok(id.clone());
            }
        }
        for summary in self.sessions(&self.project)? {
            if self
                .document(&summary.id)?
                .session
                .turns
                .iter()
                .any(|t| t.id == turn)
            {
                return Ok(summary.id);
            }
        }
        bail!("Pedido não encontrado nesta conversa")
    }

    pub(crate) fn activity(&self, turn: &str) -> Result<Option<ActivitySnapshot>> {
        Ok(self
            .document(&self.session_for_turn(turn)?)?
            .activities
            .remove(turn))
    }

    pub(crate) fn agent_messages(&self, turn: &str) -> Result<Vec<AgentMessage>> {
        Ok(self
            .document(&self.session_for_turn(turn)?)?
            .messages
            .remove(turn)
            .unwrap_or_default())
    }

    pub(crate) fn record_agent_message(
        &self,
        session: &str,
        turn: &str,
        message: &AgentMessage,
    ) -> Result<()> {
        ensure!(
            !message.message.trim().is_empty() && message.message.len() <= 64 * 1024,
            "Escreva uma orientação de até 64 KiB"
        );
        let doc = self.document(session)?;
        ensure!(
            doc.session.turns.iter().any(|t| t.id == turn),
            "Pedido não encontrado nesta conversa"
        );
        ensure!(
            !doc.messages.values().flatten().any(|m| m.id == message.id),
            "Intervenção já registrada"
        );
        self.append(
            session,
            Event::AgentMessage {
                turn_id: turn.into(),
                message: message.clone(),
            },
        )
    }

    pub(crate) fn update_agent_message(
        &self,
        turn: &str,
        id: &str,
        status: &str,
        detail: &str,
    ) -> Result<()> {
        let session = self.session_for_turn(turn)?;
        let mut message = self
            .agent_messages(turn)?
            .into_iter()
            .find(|m| m.id == id)
            .context("Intervenção não encontrada neste pedido")?;
        message.status = status.into();
        message.detail = detail.into();
        self.append(
            &session,
            Event::AgentMessage {
                turn_id: turn.into(),
                message,
            },
        )
    }

    pub(crate) fn finish_session(&mut self, id: &str) -> Result<()> {
        if self.owned.borrow().contains_key(id) {
            self.append(id, Event::Finished { at: Utc::now() })?;
            if let Some((lock, _)) = self.owned.borrow_mut().remove(id) {
                // Explicit unlock also releases a descriptor briefly inherited
                // by a concurrently spawning child before its exec closes it.
                lock.unlock()?;
            }
        }
        Ok(())
    }

    pub(crate) fn load_session(&mut self, id: &str, project: &Path) -> Result<StoredSession> {
        self.check_project(project)?;
        if self.owned.borrow().contains_key(id) {
            return self.read_session(id);
        }
        let lock = self.lock(id)?;
        let path = self.path(id)?;
        let (document, valid_bytes) = read_document(&path)?;
        ensure!(
            document.session.summary.id == id,
            "ID divergente no arquivo da conversa"
        );
        let len = fs::metadata(&path)?.len();
        if valid_bytes < len {
            // Save the original before removing an interrupted final record.
            fs::copy(
                &path,
                path.with_extension(format!("jsonl.recovery-{}", uuid::Uuid::new_v4())),
            )?;
            let file = OpenOptions::new().write(true).open(&path)?;
            file.set_len(valid_bytes)?;
            file.sync_all()?;
            self.warnings
                .borrow_mut()
                .push("Última gravação incompleta recuperada; arquivo original preservado.".into());
        }
        self.owned.borrow_mut().insert(id.into(), (lock, document));
        if let Err(error) = self.append(id, Event::Resumed { at: Utc::now() }) {
            self.owned.borrow_mut().remove(id);
            return Err(error);
        }
        self.read_session(id)
    }

    pub(crate) fn read_session(&self, id: &str) -> Result<StoredSession> {
        Ok(self.document(id)?.session)
    }

    pub(crate) fn export_session(&self, id: &str, destination: &Path) -> Result<()> {
        let document = self.document(id)?;
        let parent = destination.parent().context("Destino inválido")?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        self.write_record(
            &mut temp,
            id,
            Event::Snapshot {
                document: Box::new(document),
            },
        )?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(destination)
            .map_err(|error| error.error)
            .context("Não foi possível exportar; escolha um arquivo que ainda não existe")?;
        Ok(())
    }
}

impl Drop for ChatHistory {
    fn drop(&mut self) {
        for (lock, _) in self.owned.get_mut().values() {
            let _ = lock.unlock();
        }
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn title(prompt: &str) -> String {
    let value = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let short: String = value.chars().take(60).collect();
    if short.is_empty() {
        EMPTY_TITLE.into()
    } else if value.chars().count() > 60 {
        format!("{short}…")
    } else {
        short
    }
}

fn read_document(path: &Path) -> Result<(Document, u64)> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = Vec::new();
    let mut document = None;
    let mut identity = None;
    let mut offset = 0_u64;
    let mut number = 0;
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        number += 1;
        // An append only becomes committed when its terminating newline exists.
        if !line.ends_with(b"\n") {
            break;
        }
        let record: Record = serde_json::from_slice(&line)
            .with_context(|| format!("JSONL inválido na linha {number}"))?;
        ensure!(
            record.format == FORMAT && record.version == VERSION,
            "Formato ou versão JSONL não suportado na linha {number}"
        );
        ensure!(
            valid_id(&record.session_id),
            "ID inválido na linha {number}"
        );
        if let Some((project, session)) = &identity {
            ensure!(
                project == &record.project_id && session == &record.session_id,
                "Identidade divergente na linha {number}"
            );
        } else {
            identity = Some((record.project_id, record.session_id.clone()));
        }
        match (&mut document, record.event) {
            (None, Event::Snapshot { document: first }) => {
                ensure!(
                    first.session.summary.id == record.session_id,
                    "ID divergente no snapshot"
                );
                validate_document(&first)?;
                document = Some(*first);
            }
            (Some(current), event) => apply(current, &event)?,
            _ => bail!("O JSONL precisa começar com um snapshot da sessão"),
        }
        offset += n as u64;
    }
    Ok((
        document.context("Arquivo sem uma sessão StackPulse completa")?,
        offset,
    ))
}

fn validate_document(document: &Document) -> Result<()> {
    let turns = &document.session.turns;
    let ids: std::collections::HashSet<_> = turns.iter().map(|t| &t.id).collect();
    ensure!(ids.len() == turns.len(), "Pedidos duplicados no histórico");
    ensure!(
        document.session.summary.turns == turns.len(),
        "Contagem de pedidos inválida"
    );
    ensure!(
        document
            .activities
            .keys()
            .chain(document.messages.keys())
            .all(|id| ids.contains(id)),
        "Atividade sem pedido correspondente"
    );
    Ok(())
}

fn apply(document: &mut Document, event: &Event) -> Result<()> {
    match event {
        Event::Snapshot { .. } => bail!("Snapshot duplicado no JSONL"),
        Event::Renamed { title } => {
            document.session.summary.title = title.clone();
            document.session.summary.custom_title = true;
        }
        Event::Started { turn, at } => {
            ensure!(
                !document.session.turns.iter().any(|t| t.id == turn.id),
                "Pedido duplicado"
            );
            let summary = &mut document.session.summary;
            if summary.turns == 0 && !summary.custom_title && summary.title == EMPTY_TITLE {
                summary.title = title(&turn.prompt);
            }
            document.session.turns.push((**turn).clone());
            summary.turns = document.session.turns.len();
            summary.updated_at = *at;
            summary.ended_at = None;
        }
        Event::Updated {
            turn_id,
            keep_lines,
            append_lines,
            status,
            execution,
            ended_at,
            at,
        } => {
            let turn = document
                .session
                .turns
                .iter_mut()
                .find(|t| &t.id == turn_id)
                .context("Pedido ausente no JSONL")?;
            ensure!(
                *keep_lines <= turn.response.len(),
                "Delta de resposta inválido"
            );
            turn.response.truncate(*keep_lines);
            turn.response.extend(append_lines.iter().cloned());
            turn.status = status.clone();
            turn.execution = execution.as_deref().cloned();
            turn.ended_at = *ended_at;
            document.session.summary.updated_at = *at;
        }
        Event::Activity { turn_id, snapshot } => {
            ensure!(
                document.session.turns.iter().any(|t| &t.id == turn_id),
                "Pedido ausente no JSONL"
            );
            document
                .activities
                .insert(turn_id.clone(), snapshot.clone());
        }
        Event::AgentMessage { turn_id, message } => {
            ensure!(
                document.session.turns.iter().any(|t| &t.id == turn_id),
                "Pedido ausente no JSONL"
            );
            let messages = document.messages.entry(turn_id.clone()).or_default();
            if let Some(current) = messages.iter_mut().find(|m| m.id == message.id) {
                *current = message.clone();
            } else {
                messages.push(message.clone());
            }
        }
        Event::Finished { at } => {
            document.session.summary.updated_at = *at;
            document.session.summary.ended_at = Some(*at);
        }
        Event::Resumed { at } => {
            document.session.summary.updated_at = *at;
            document.session.summary.ended_at = None;
            for turn in &mut document.session.turns {
                if turn.ended_at.is_none() {
                    turn.ended_at = Some(*at);
                    turn.status = INTERRUPTED.into();
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "chat_history_tests.rs"]
mod tests;
