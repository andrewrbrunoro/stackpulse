//! Explicit, offline transcript import. Source files are never modified or executed.
use super::*;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) struct ImportResult {
    pub id: String,
    pub client: String,
    pub turns: usize,
    pub duplicate: bool,
}

impl ChatHistory {
    pub(crate) fn import_session(&self, source: &Path) -> Result<ImportResult> {
        ensure!(
            fs::metadata(source)?.len() <= 128 * 1024 * 1024,
            "Importe arquivos JSONL de até 128 MiB"
        );
        let content = fs::read_to_string(source).context("O histórico precisa ser texto UTF-8")?;
        let mut values = Vec::new();
        for (i, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            values.push(serde_json::from_str::<Value>(line).with_context(|| {
                format!(
                    "JSONL inválido na linha {}. Exporte novamente quando a gravação terminar.",
                    i + 1
                )
            })?);
        }
        ensure!(!values.is_empty(), "Arquivo JSONL vazio");
        let mut document = if values[0]["format"] == FORMAT {
            // Exports from other tools may omit the final newline; our own parser
            // treats that as an uncommitted append, so validate a normalized copy.
            let mut temp = tempfile::NamedTempFile::new()?;
            for value in &values {
                serde_json::to_writer(&mut temp, value)?;
                temp.write_all(b"\n")?;
            }
            let (mut doc, _) = read_document(temp.path())?;
            doc.origin.get_or_insert_with(|| Origin {
                client: "stackpulse".into(),
                session_id: doc.session.summary.id.clone(),
            });
            doc
        } else {
            native_document(&values)?
        };
        let origin = document.origin.as_ref().unwrap();
        let key = format!("{}\0{}", origin.client, origin.session_id);
        let id = format!("import-{:x}", Sha256::digest(key.as_bytes()));
        let result = ImportResult {
            id: id.clone(),
            client: origin.client.clone(),
            turns: document.session.turns.len(),
            duplicate: self.path(&id)?.exists(),
        };
        if result.duplicate {
            self.document(&id)?;
            return Ok(result);
        }
        let _lock = self.lock(&id)?;
        if self.path(&id)?.exists() {
            self.document(&id)?;
            return Ok(ImportResult {
                duplicate: true,
                ..result
            });
        }
        // Keep origins in metadata, never use client-controlled IDs as paths.
        document.session.summary.id = id.clone();
        document.session.summary.ended_at = Some(document.session.summary.updated_at);
        for (index, turn) in document.session.turns.iter_mut().enumerate() {
            let old_id = std::mem::replace(&mut turn.id, format!("{id}-t{index}"));
            if let Some(activity) = document.activities.remove(&old_id) {
                document.activities.insert(turn.id.clone(), activity);
            }
            if let Some(messages) = document.messages.remove(&old_id) {
                document.messages.insert(turn.id.clone(), messages);
            }
            if turn.ended_at.is_none() {
                turn.ended_at = Some(document.session.summary.updated_at);
                turn.status = "Importado · execução incompleta na origem".into();
            }
        }
        validate_document(&document)?;
        self.write_new(&document)?;
        Ok(result)
    }
}

fn native_document(values: &[Value]) -> Result<Document> {
    let client = if values.iter().any(|v| v["type"] == "session_meta") {
        "codex"
    } else if values
        .iter()
        .any(|v| v["sessionId"].is_string() && (v["type"] == "user" || v["type"] == "assistant"))
    {
        "claude"
    } else {
        bail!(
            "Formato não reconhecido. Use JSONL do StackPulse, rollout do Codex ou transcript do Claude Code."
        );
    };
    let native_ids: std::collections::HashSet<_> = values
        .iter()
        .filter_map(|v| {
            if client == "codex" && v["type"] == "session_meta" {
                v["payload"]["id"].as_str()
            } else if client == "claude" {
                v["sessionId"].as_str()
            } else {
                None
            }
        })
        .collect();
    ensure!(
        native_ids.len() == 1,
        "O arquivo precisa conter exatamente uma sessão do cliente"
    );
    let native_id = native_ids.into_iter().next().unwrap().to_string();
    let dates: Vec<DateTime<Utc>> = values
        .iter()
        .filter_map(|v| v["timestamp"].as_str()?.parse().ok())
        .collect();
    let created = dates
        .iter()
        .min()
        .copied()
        .context("Histórico sem data de sessão válida")?;
    let updated = dates.iter().max().copied().unwrap_or(created);
    let use_items = values
        .iter()
        .any(|v| v["type"] == "response_item" && v["payload"]["type"] == "message");
    let mut turns: Vec<StoredTurn> = vec![];
    let last_uuid: HashMap<_, _> = values
        .iter()
        .enumerate()
        .filter_map(|(index, value)| value["uuid"].as_str().map(|id| (id, index)))
        .collect();
    let mut pending = vec![];
    let mut custom_title = None;
    for (index, value) in values.iter().enumerate() {
        if let Some(name) = value["customTitle"]
            .as_str()
            .or_else(|| value["summary"].as_str())
        {
            custom_title = Some(crate::chat_agents::sanitize(name));
        }
        // Keep the last complete version of a repeated Claude message UUID.
        if let Some(id) = value["uuid"].as_str()
            && last_uuid.get(id).copied() != Some(index)
        {
            continue;
        }
        let (role, content) = if client == "claude" {
            if value["isMeta"] == true || value["isSidechain"] == true {
                ("", String::new())
            } else {
                (
                    value["message"]["role"].as_str().unwrap_or(""),
                    content_text(&value["message"]["content"]),
                )
            }
        } else if use_items
            && value["type"] == "response_item"
            && value["payload"]["type"] == "message"
        {
            (
                value["payload"]["role"].as_str().unwrap_or(""),
                content_text(&value["payload"]["content"]),
            )
        } else if !use_items && value["type"] == "event_msg" {
            let payload = &value["payload"];
            match payload["type"].as_str() {
                Some("user_message") => ("user", payload["message"].as_str().unwrap_or("").into()),
                Some("agent_message") => (
                    "assistant",
                    payload["message"].as_str().unwrap_or("").into(),
                ),
                _ => ("", String::new()),
            }
        } else {
            ("", String::new())
        };
        let at = value["timestamp"]
            .as_str()
            .and_then(|v| v.parse::<DateTime<Utc>>().ok());
        if role == "user" && !content.trim().is_empty() {
            if let Some(previous) = turns.last_mut() {
                previous.ended_at = previous.ended_at.or(at);
            }
            turns.push(StoredTurn {
                id: uuid::Uuid::new_v4().to_string(),
                prompt: content,
                response: vec![],
                status: format!("Importado · {client}"),
                profile: format!("Histórico {client}"),
                execution: None,
                started_at: at,
                ended_at: None,
                source_events: std::mem::take(&mut pending),
            });
        } else if role == "assistant"
            && !content.trim().is_empty()
            && let Some(turn) = turns.last_mut()
        {
            turn.response.extend(content.lines().map(str::to_owned));
            turn.ended_at = at;
        }
        if let Some(turn) = turns.last_mut() {
            turn.source_events.push(value.clone());
        } else {
            pending.push(value.clone());
        }
    }
    ensure!(
        !turns.is_empty(),
        "Nenhum pedido do usuário encontrado neste histórico"
    );
    if let Some(turn) = turns.last_mut() {
        turn.source_events.extend(pending);
    }
    let has_title = custom_title.as_ref().is_some_and(|t| !t.is_empty());
    let title = custom_title
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| title(&turns[0].prompt));
    Ok(Document {
        session: StoredSession {
            summary: SessionSummary {
                id: native_id.clone(),
                title,
                custom_title: has_title,
                created_at: created,
                updated_at: updated,
                ended_at: Some(updated),
                turns: turns.len(),
            },
            turns,
        },
        activities: HashMap::new(),
        messages: HashMap::new(),
        origin: Some(Origin {
            client: client.into(),
            session_id: native_id,
        }),
    })
}

fn content_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    content
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| match part["type"].as_str() {
                    Some("text" | "input_text" | "output_text") => {
                        part["text"].as_str().map(str::to_string)
                    }
                    Some("image" | "input_image") => {
                        Some("[Imagem referenciada no histórico de origem]".into())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}
