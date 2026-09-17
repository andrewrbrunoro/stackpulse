//! Profile-first terminal for tracked requests with per-session conversation context.
use crate::{
    activity::{ActivitySnapshot, AgentStatus},
    app_ui::{self, Options, emitted_execution_id, path_option},
    chat_agent_detail::{self, DetailAction, DetailEntry, DetailView},
    chat_agents,
    chat_composer::Composer,
    chat_history::{AgentMessage, ChatHistory, SessionSummary, StoredSession, StoredTurn},
    chat_metrics::{ConversationStats, PromptStats},
    chat_profiles::{self, Catalog, Choice},
    chat_tabs::{self, Rect, TabAction, TabLabel},
    chat_widgets::{self, WidgetAction},
    db::Db,
    tui::{self, Frame, TerminalGuard, Tone},
    ui_commands::{self, Action, Form},
    ui_data::Page,
    ui_job::Job,
    widget,
    workflow::{self, Execution, Feedback},
};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use crossterm::{
    event::{
        self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    terminal,
};
use rusqlite::OptionalExtension;
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

fn open_response_browser(path: &std::path::Path) -> std::io::Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer.exe"
    } else {
        "xdg-open"
    };
    let mut child = std::process::Command::new(program)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

const COMMANDS: [(&str, &str); 27] = [
    ("/title", "editar o título desta conversa"),
    (
        "/settings",
        "configurações de perfil, equipe, agentes e execução",
    ),
    ("/agents", "ver os subagentes e suas atividades"),
    ("/team", "ver todos os papéis da equipe configurada"),
    ("/profile", "trocar a equipe"),
    ("/options", "permissões, benchmark e limite"),
    (
        "/skip-dangerous",
        "sem sandbox/aprovações: on ativa, off restaura",
    ),
    ("/no-policy", "alias de /skip-dangerous [on|off]"),
    (
        "/dangerously-skip-permissions",
        "alias de /skip-dangerous [on|off]",
    ),
    ("/feedback", "avaliar o pedido selecionado"),
    ("/usage", "tokens e avaliações de cada pedido da conversa"),
    ("/history", "execuções e consumo"),
    ("/sessions", "conversas JSONL deste projeto"),
    ("/import-session", "importar conversa de outro cliente"),
    ("/export-session", "exportar esta conversa em JSONL"),
    ("/new", "iniciar uma nova conversa"),
    ("/trend", "curvas de entrega"),
    ("/report", "relatório de tokens e tempo"),
    ("/setup", "configurar o auxiliar"),
    (
        "/update",
        "atualizar o StackPulse a partir dos fontes locais",
    ),
    ("/menu", "todos os comandos"),
    ("/widget", "widget compacto"),
    ("/clear", "limpar a conversa da tela"),
    ("/credits", "opcionais, autoria e links dos projetos"),
    ("/help", "ajuda e atalhos"),
    ("/preview", "prévia: /preview seu pedido"),
    ("/exit", "sair do terminal"),
];

const SETTINGS: [(&str, &str, &str); 8] = [
    ("Perfil", "Escolher a equipe desta conversa", "/profile"),
    (
        "Equipe",
        "Papéis, delegação e integração do perfil",
        "/team",
    ),
    (
        "Opções de execução",
        "Permissões, limite e benchmark",
        "/options",
    ),
    (
        "Agentes e modelos",
        "Gerenciar os perfis e seus agentes",
        "/manage-profiles",
    ),
    (
        "Auxiliar e plugins",
        "Providers, memória e opcionais",
        "/setup",
    ),
    (
        "Conversas salvas",
        "Reabrir o histórico deste projeto",
        "/sessions",
    ),
    (
        "Consumo e relatórios",
        "Execuções, tokens e avaliações",
        "/history",
    ),
    (
        "Créditos",
        "Autoria e links dos projetos opcionais",
        "/credits",
    ),
];

enum Stage {
    Profiles,
    Chat,
    Sessions(SessionPicker),
    Feedback(FeedbackPicker),
    Form(ChatForm),
}
struct SessionPicker {
    sessions: Vec<SessionSummary>,
    selected: usize,
}
struct FeedbackPicker {
    selected: usize,
    scroll: usize,
}
#[derive(Clone)]
enum Transfer {
    Import,
    Export(String),
}
struct ChatForm {
    renaming: bool,
    transfer: Option<Transfer>,
    form: Form,
    indices: Vec<usize>,
    inputs: Vec<Composer>,
    selected: usize,
    error: String,
}
struct Turn {
    id: String,
    prompt: String,
    profile: String,
    lines: Vec<String>,
    status: String,
    execution: Option<Execution>,
    finished: bool,
}
impl From<StoredTurn> for Turn {
    fn from(turn: StoredTurn) -> Self {
        Self {
            id: turn.id,
            prompt: turn.prompt,
            profile: turn.profile,
            lines: turn.response,
            status: turn.status,
            execution: turn.execution,
            finished: turn.ended_at.is_some(),
        }
    }
}
#[derive(Default)]
struct AgentViewState {
    selected: Option<String>,
    drafts: HashMap<String, Composer>,
    messages: Vec<AgentMessage>,
    pending: HashSet<String>,
    scroll: usize,
    focused: bool,
}

struct Running {
    job: Job,
    approval_view: Option<ApprovalView>,
    preview: bool,
    turn: usize,
    emitted: Option<String>,
    last_metrics: Instant,
    last_history: Instant,
}

struct ApprovalView {
    request: crate::agent_control::ApprovalRequest,
    scroll: usize,
    visible: bool,
}
// A parked tab owns its complete conversation and child process. The SQLite
// connection and its ownership token are shared by every tab in this terminal.
struct TabState {
    session: SessionSummary,
    search: Composer,
    pick: usize,
    chosen: Option<Choice>,
    stage: Stage,
    composer: Composer,
    run_form: Form,
    turns: Vec<Turn>,
    hidden_turns: usize,
    metric_pick: Option<usize>,
    metrics_open: bool,
    running: Option<Running>,
    activity: ActivitySnapshot,
    agent_scroll: usize,
    agent_view: AgentViewState,
    agents_expanded: bool,
    note: Vec<String>,
    focus_note: bool,
    notice: String,
    scroll: usize,
    follow: bool,
    command_pick: usize,
    history_pick: Option<usize>,
    unread: bool,
}
impl TabState {
    fn new(session: SessionSummary, chosen: Option<Choice>, run_form: Form) -> Self {
        Self {
            session,
            search: Composer::default(),
            pick: 0,
            stage: if chosen.is_some() {
                Stage::Chat
            } else {
                Stage::Profiles
            },
            chosen,
            composer: Composer::default(),
            run_form,
            turns: vec![],
            hidden_turns: 0,
            metric_pick: None,
            metrics_open: false,
            running: None,
            activity: ActivitySnapshot::default(),
            agent_scroll: 0,
            agent_view: AgentViewState::default(),
            agents_expanded: false,
            note: vec![],
            focus_note: false,
            notice: String::new(),
            scroll: 0,
            follow: true,
            command_pick: 0,
            history_pick: None,
            unread: false,
        }
    }
}
#[derive(Clone)]
enum ClickAction {
    Tab(TabAction),
    Widget(WidgetAction),
    Profile(usize),
    Session(String),
    ImportSession,
    ExportSession,
    Feedback(usize),
    FeedbackSubmit,
    FeedbackContent,
    FormField(usize, usize),
    Save,
    Suggestion(&'static str),
    Composer { first: usize, width: usize },
    Submit,
    Cancel,
    Transcript,
    Sessions,
    Agents,
    Search(usize),
    Back,
    Setting(usize),
    Agent(String),
    AgentDetail(DetailAction),
}

enum TranscriptLine {
    Text(String, Tone),
}
struct Chat {
    browser_opener: fn(&std::path::Path) -> std::io::Result<()>,
    selecting_text: bool,
    options: Options,
    cwd: PathBuf,
    executable: PathBuf,
    catalog: Catalog,
    history: ChatHistory,
    session: SessionSummary,
    search: Composer,
    pick: usize,
    chosen: Option<Choice>,
    stage: Stage,
    composer: Composer,
    run_form: Form,
    turns: Vec<Turn>,
    hidden_turns: usize,
    metric_pick: Option<usize>,
    metrics_open: bool,
    running: Option<Running>,
    activity: ActivitySnapshot,
    agent_scroll: usize,
    agent_view: AgentViewState,
    agents_expanded: bool,
    note: Vec<String>,
    focus_note: bool,
    notice: String,
    scroll: usize,
    follow: bool,
    command_pick: usize,
    history_pick: Option<usize>,
    unread: bool,
    tabs: Vec<Option<TabState>>,
    active_tab: usize,
    settings_open: bool,
    settings_pick: usize,
    clicks: Vec<(Rect, ClickAction)>,
    metrics_version: Option<i64>,
    metrics_checked: Instant,
    interrupted: Arc<AtomicBool>,
}
impl Chat {
    fn new(options: Options, cwd: PathBuf) -> Result<Self> {
        let catalog = chat_profiles::load(&options.config, &cwd);
        let pick = catalog.selected;
        let mut history = ChatHistory::open(&cwd, &options.db)?;
        let session = history.create_session(&cwd)?;
        let notice = history.take_warnings();
        Ok(Self {
            browser_opener: open_response_browser,
            selecting_text: false,
            options,
            cwd,
            executable: std::env::current_exe()?,
            catalog,
            history,
            session,
            search: Composer::default(),
            pick,
            chosen: None,
            stage: Stage::Profiles,
            composer: Composer::default(),
            run_form: ui_commands::form(Action::Run, None),
            turns: vec![],
            hidden_turns: 0,
            metric_pick: None,
            metrics_open: false,
            running: None,
            activity: ActivitySnapshot::default(),
            agent_scroll: 0,
            agent_view: AgentViewState::default(),
            agents_expanded: false,
            note: vec![],
            focus_note: false,
            notice,
            scroll: 0,
            follow: true,
            command_pick: 0,
            history_pick: None,
            unread: false,
            tabs: vec![None],
            active_tab: 0,
            settings_open: false,
            settings_pick: 0,
            clicks: vec![],
            metrics_version: None,
            metrics_checked: Instant::now(),
            interrupted: Arc::new(AtomicBool::new(false)),
        })
    }
    fn choices(&self) -> Vec<usize> {
        let search = self.search.value.to_lowercase();
        self.catalog
            .choices
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                format!("{} {} {}", c.name, c.scope, c.subtitle)
                    .to_lowercase()
                    .contains(&search)
            })
            .map(|(i, _)| i)
            .collect()
    }
    fn setting(
        &mut self,
        index: usize,
        db: &mut Db,
        guard: &mut Option<TerminalGuard>,
    ) -> Result<()> {
        let Some((_, _, command)) = SETTINGS.get(index) else {
            return Ok(());
        };
        let command = *command;
        ensure!(
            self.running.is_none() || matches!(command, "/team" | "/sessions" | "/credits"),
            "Aguarde o pedido desta aba terminar para alterar sua configuração."
        );
        let draft = std::mem::take(&mut self.composer);
        let result = if command == "/manage-profiles" {
            self.navigate(Page::Profiles, db, guard).map(|_| false)
        } else {
            self.composer.value = command.into();
            self.command(db, guard)
        };
        self.composer = draft;
        if result.is_ok() {
            self.settings_open = false;
            self.agent_view.selected = None;
            self.agent_view.focused = false;
            if matches!(command, "/team" | "/credits") {
                self.stage = Stage::Chat;
            }
        }
        result.map(|_| ())
    }

    fn draw_settings(&mut self, frame: &mut Frame, top: usize) {
        let width = frame.width.saturating_sub(4);
        let height = frame.height;
        put(frame, 2, top, "CONFIGURAÇÕES", Tone::Highlight, width);
        if let Some(choice) = &self.chosen {
            put(
                frame,
                2,
                top + 1,
                &choice.team_summary(),
                Tone::Muted,
                width,
            );
        }
        let capacity = height
            .saturating_sub(top + 5)
            .checked_div(2)
            .unwrap_or(0)
            .max(1);
        self.settings_pick = self.settings_pick.min(SETTINGS.len() - 1);
        let first = self.settings_pick.saturating_sub(capacity - 1);
        for (offset, (label, description, _)) in
            SETTINGS.iter().skip(first).take(capacity).enumerate()
        {
            let index = first + offset;
            let y = top + 2 + offset * 2;
            let selected = index == self.settings_pick;
            let title_width = width.min(26);
            put(
                frame,
                2,
                y,
                &format!("{} {label}", if selected { "❯" } else { " " }),
                if selected {
                    Tone::Selected
                } else {
                    Tone::Navigation
                },
                title_width,
            );
            put(
                frame,
                2 + title_width,
                y,
                description,
                Tone::Muted,
                width.saturating_sub(title_width),
            );
            target(
                &mut self.clicks,
                2,
                y,
                width,
                1,
                ClickAction::Setting(index),
            );
        }
        put(frame, 2, height - 3, &self.notice, Tone::Warning, width);
        put(
            frame,
            2,
            height - 2,
            "↑↓ escolher · Enter abrir · Esc voltar à conversa · F6 abas",
            Tone::Muted,
            width,
        );
    }

    fn open_agent(&mut self, id: String) {
        if self.activity.agents.iter().any(|agent| agent.id == id) {
            self.agent_view.drafts.entry(id.clone()).or_default();
            self.agent_view.selected = Some(id);
            self.agent_view.focused = true;
            self.agent_view.scroll = 0;
        }
    }

    fn close_agent(&mut self) {
        self.agent_view.selected = None;
        self.agent_view.focused = false;
        self.agents_expanded = false;
    }

    fn send_agent(&mut self, redirect: bool) -> Result<()> {
        let agent_id = self
            .agent_view
            .selected
            .clone()
            .context("Abra um subagente primeiro")?;
        ensure!(
            self.activity
                .agents
                .iter()
                .any(|agent| agent.id == agent_id),
            "Subagente indisponível nesta execução"
        );
        let message = self
            .agent_view
            .drafts
            .get(&agent_id)
            .map(|draft| draft.value.clone())
            .unwrap_or_default();
        ensure!(
            !message.trim().is_empty(),
            "Escreva uma orientação antes de enviar."
        );
        ensure!(
            message.len() <= 16 * 1024,
            "A orientação pode ter até 16 KiB. O rascunho foi preservado."
        );
        ensure!(
            !self
                .agent_view
                .messages
                .iter()
                .any(|message| message.agent_id == agent_id
                    && self.agent_view.pending.contains(&message.id)),
            "Aguarde a confirmação da intervenção anterior neste agente."
        );
        let running = self
            .running
            .as_mut()
            .context("Execução encerrada. A atividade está disponível para consulta.")?;
        let capability = running.job.agent_capability();
        ensure!(
            if redirect {
                capability.can_interrupt
            } else {
                capability.can_send
            },
            "{}",
            capability.note
        );
        let turn = &self.turns[running.turn];
        let id = uuid::Uuid::new_v4().to_string();
        let mut entry = AgentMessage {
            id: id.clone(),
            agent_id: agent_id.clone(),
            redirect,
            message,
            status: "Preparando envio".into(),
            detail: "Intervenção registrada; aguardando o canal do provider.".into(),
        };
        // Persist intent before the runtime can deliver it. A failure preserves the draft.
        self.history
            .record_agent_message(&self.session.id, &turn.id, &entry)?;
        if let Err(error) = running
            .job
            .send_agent_control(&id, &agent_id, &entry.message, redirect)
        {
            entry.status = "Falhou".into();
            entry.detail = format!("{error:#}");
            self.history
                .update_agent_message(&turn.id, &id, &entry.status, &entry.detail)?;
            self.agent_view.messages.push(entry);
            return Err(error);
        }
        self.agent_view.pending.insert(id);
        self.agent_view.messages.push(entry);
        self.agent_view.drafts.insert(agent_id, Composer::default());
        self.agent_view.scroll = usize::MAX;
        self.notice = "Intervenção registrada · aguardando confirmação do orquestrador.".into();
        Ok(())
    }

    fn poll_agent_receipts(&mut self) {
        let Some(running) = &self.running else {
            return;
        };
        let turn_id = &self.turns[running.turn].id;
        // The transport retains transitions for auditing. Apply only its latest
        // state per request; replaying older states would overwrite notices and
        // regress delivery state on every frame.
        let mut seen = HashSet::new();
        let latest = running
            .job
            .control_receipts()
            .into_iter()
            .rev()
            .filter(|receipt| seen.insert(receipt.request_id.clone()))
            .collect::<Vec<_>>();
        for receipt in latest.into_iter().rev() {
            let Some(message) = self
                .agent_view
                .messages
                .iter_mut()
                .find(|message| message.id == receipt.request_id)
            else {
                continue;
            };
            let status = receipt.status.label();
            if message.status == status && message.detail == receipt.detail {
                continue;
            }
            if let Err(error) =
                self.history
                    .update_agent_message(turn_id, &message.id, status, &receipt.detail)
            {
                self.notice = format!("Falha ao registrar confirmação: {error:#}");
                continue;
            }
            message.status = status.into();
            message.detail = receipt.detail;
            if receipt.status.terminal() {
                self.agent_view.pending.remove(&message.id);
            }
            self.notice = format!("Subagente · {status}");
        }
    }

    fn finish_agent_receipts(&mut self) -> Result<()> {
        self.poll_agent_receipts();
        let Some(turn_id) = self
            .running
            .as_ref()
            .map(|running| self.turns[running.turn].id.clone())
        else {
            return Ok(());
        };
        for message in &mut self.agent_view.messages {
            if self.agent_view.pending.contains(&message.id) {
                let detail = format!(
                    "Execução encerrada sem confirmação final. Último estado: {}. {}",
                    message.status, message.detail
                );
                self.history.update_agent_message(
                    &turn_id,
                    &message.id,
                    "Sem confirmação",
                    &detail,
                )?;
                message.status = "Sem confirmação".into();
                message.detail = detail;
            }
        }
        self.agent_view.pending.clear();
        Ok(())
    }

    fn draw_agent_detail(&mut self, frame: &mut Frame, area: Rect) {
        let Some(id) = self.agent_view.selected.as_ref() else {
            return;
        };
        let Some(agent) = self.activity.agents.iter().find(|agent| &agent.id == id) else {
            put(
                frame,
                area.x,
                area.y,
                "Subagente indisponível · Esc volta à equipe",
                Tone::Warning,
                area.width,
            );
            return;
        };
        let capability = self
            .running
            .as_ref()
            .map(|running| running.job.agent_capability());
        let can_send = capability
            .as_ref()
            .is_some_and(|capability| capability.can_send);
        let can_interrupt = capability
            .as_ref()
            .is_some_and(|capability| capability.can_interrupt);
        let note = if can_send || can_interrupt {
            "O orquestrador recebe e encaminha sua orientação."
        } else {
            capability
                .as_ref()
                .map(|capability| capability.note.as_str())
                .unwrap_or("Execução encerrada · atividade disponível para consulta.")
        };
        let entries = self
            .agent_view
            .messages
            .iter()
            .filter(|message| &message.agent_id == id)
            .map(|message| DetailEntry {
                title: format!(
                    "Você · {} · {}",
                    if message.redirect {
                        "redirecionamento"
                    } else {
                        "orientação"
                    },
                    message.status
                ),
                text: if matches!(
                    message.status.as_str(),
                    "Falhou" | "Sem confirmação" | "Indisponível"
                ) {
                    format!("{}\n{}", message.message, message.detail)
                } else {
                    message.message.clone()
                },
            })
            .collect::<Vec<_>>();
        let busy = self.agent_view.messages.iter().any(|message| {
            &message.agent_id == id && self.agent_view.pending.contains(&message.id)
        });
        let composer = self.agent_view.drafts.entry(id.clone()).or_default();
        let hits = chat_agent_detail::draw(
            frame,
            area,
            DetailView {
                agent,
                entries: &entries,
                composer,
                can_send,
                can_interrupt,
                capability_note: note,
                busy,
                focused: self.agent_view.focused,
            },
            &mut self.agent_view.scroll,
        );
        self.clicks.extend(
            hits.into_iter()
                .map(|(rect, action)| (rect, ClickAction::AgentDetail(action))),
        );
    }
    fn choose(&mut self) {
        let Some(index) = self.choices().get(self.pick).copied() else {
            self.notice =
                "Nenhum perfil para selecionar. F2 configura o auxiliar; F3 abre os perfis.".into();
            return;
        };
        let choice = &self.catalog.choices[index];
        if let Some(problem) = &choice.problem {
            self.notice.clone_from(problem);
            return;
        }
        self.chosen = Some(choice.clone());
        self.activity = ActivitySnapshot::default();
        self.agent_scroll = 0;
        self.agent_view = AgentViewState::default();
        self.run_form.fields[1].value.clone_from(&choice.name);
        self.stage = Stage::Chat;
        self.note.clear();
        self.focus_note = false;
        self.notice.clear();
    }
    fn refresh_profiles(&mut self) {
        self.catalog = chat_profiles::load(&self.options.config, &self.cwd);
        self.search = Composer::default();
        self.pick = self
            .chosen
            .as_ref()
            .and_then(|c| self.catalog.choices.iter().position(|v| v.name == c.name))
            .unwrap_or(self.catalog.selected);
    }
    fn show_sessions(&mut self) -> Result<()> {
        let sessions = self.history.sessions(&self.cwd)?;
        let selected = sessions
            .iter()
            .position(|session| session.id == self.session.id)
            .unwrap_or_default();
        self.stage = Stage::Sessions(SessionPicker { sessions, selected });
        self.notice = self.history.take_warnings();
        Ok(())
    }
    fn load_session(&mut self, id: &str) -> Result<()> {
        self.metrics_version = None;
        if id == self.session.id {
            self.hidden_turns = 0;
            if self.turns.is_empty() && self.running.is_none() {
                // Reopening a cleared view is read-only; never interrupt an
                // owned running session by calling load_session again.
                let saved = self.history.read_session(id)?;
                self.session = saved.summary;
                self.turns = saved.turns.into_iter().map(Turn::from).collect();
            }
            if matches!(self.stage, Stage::Sessions(_)) {
                self.stage = if self.chosen.is_some() {
                    Stage::Chat
                } else {
                    Stage::Profiles
                };
            }
            return Ok(());
        }
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.as_ref().is_some_and(|tab| tab.session.id == id))
        {
            self.switch_tab(index);
            return self.load_session(id);
        }
        let StoredSession { summary, turns } = self.history.load_session(id, &self.cwd)?;
        let chosen = turns
            .last()
            .and_then(|turn| {
                self.catalog
                    .choices
                    .iter()
                    .find(|choice| choice.name == turn.profile && choice.problem.is_none())
            })
            .cloned()
            .or_else(|| self.chosen.clone());
        let mut state = TabState::new(summary, chosen, self.run_form.clone());
        state.turns = turns.into_iter().map(Turn::from).collect();
        if let Some(turn) = state.turns.last() {
            state.activity = self.history.activity(&turn.id)?.unwrap_or_default();
            // A recovered session has no live child to time. Without an observed
            // end, keep its start metadata but report an unavailable duration.
            for agent in &mut state.activity.agents {
                if agent.finished_at.is_none() {
                    agent.status = AgentStatus::Unknown;
                }
            }
            state.agent_view.messages = self.history.agent_messages(&turn.id)?;
        }
        self.tabs.push(Some(state));
        self.switch_tab(self.tabs.len() - 1);
        self.notice = format!(
            "Conversa reaberta · {} pedidos · perfil ativo: {}",
            self.turns.len(),
            self.chosen
                .as_ref()
                .map(|choice| choice.name.as_str())
                .unwrap_or("escolha um perfil")
        );
        let warnings = self.history.take_warnings();
        if !warnings.is_empty() {
            self.notice.push_str(&format!(" · {warnings}"));
        }
        Ok(())
    }
    fn new_session(&mut self) -> Result<()> {
        let state = TabState::new(
            self.history.create_session(&self.cwd)?,
            self.chosen.clone(),
            self.run_form.clone(),
        );
        self.tabs.push(Some(state));
        self.switch_tab(self.tabs.len() - 1);
        self.notice = "Nova aba · cada conversa preserva seu perfil e rascunho.".into();
        Ok(())
    }
    fn swap_tab(&mut self, tab: &mut TabState) {
        macro_rules! swap { ($($field:ident),+ $(,)?) => { $(std::mem::swap(&mut self.$field, &mut tab.$field);)+ }; }
        swap!(
            session,
            search,
            pick,
            chosen,
            stage,
            composer,
            run_form,
            turns,
            hidden_turns,
            metric_pick,
            metrics_open,
            running,
            activity,
            agent_scroll,
            agent_view,
            agents_expanded,
            note,
            focus_note,
            notice,
            scroll,
            follow,
            command_pick,
            history_pick,
            unread
        );
    }
    fn switch_tab(&mut self, index: usize) {
        let Some(Some(mut tab)) = self.tabs.get_mut(index).map(Option::take) else {
            return;
        };
        self.swap_tab(&mut tab);
        if let Some(view) = self.running.as_mut().and_then(|r| r.approval_view.as_mut()) {
            view.visible = false;
        }
        self.tabs[self.active_tab] = Some(tab);
        self.active_tab = index;
        self.unread = false;
        self.clicks.clear();
        self.metrics_version = None;
    }
    fn tab_order(&self) -> Vec<usize> {
        let session = |index: usize| {
            if index == self.active_tab {
                &self.session
            } else {
                &self.tabs[index]
                    .as_ref()
                    .expect("parked conversation")
                    .session
            }
        };
        let mut order: Vec<_> = (0..self.tabs.len()).collect();
        order.sort_by(|a, b| {
            session(*b)
                .updated_at
                .cmp(&session(*a).updated_at)
                .then_with(|| session(*b).created_at.cmp(&session(*a).created_at))
                .then_with(|| b.cmp(a))
        });
        order
    }
    fn tab_action(&mut self, action: TabAction) -> Result<()> {
        let order = self.tab_order();
        let position = order
            .iter()
            .position(|index| *index == self.active_tab)
            .unwrap_or(0);
        let was_settings = self.settings_open;
        self.settings_open = false;
        match action {
            TabAction::Settings => self.settings_open = true,
            TabAction::Select(index) => {
                if index == self.active_tab && self.chosen.is_some() {
                    self.stage = Stage::Chat;
                }
                self.switch_tab(index);
            }
            TabAction::New => self.new_session()?,
            TabAction::Rename => self.edit_title(),
            TabAction::Previous => {
                if was_settings {
                    self.switch_tab(*order.last().unwrap());
                } else if position == 0 {
                    self.settings_open = true;
                } else {
                    self.switch_tab(order[position - 1]);
                }
            }
            TabAction::Next => {
                if was_settings {
                    self.switch_tab(order[0]);
                } else if position + 1 == self.tabs.len() {
                    self.settings_open = true;
                } else {
                    self.switch_tab(order[position + 1]);
                }
            }
            TabAction::Close if was_settings => {}
            TabAction::Close => {
                ensure!(
                    self.running.is_none(),
                    "Esta aba está trabalhando. Use Cancelar e aguarde antes de fechar."
                );
                if self.tabs.len() == 1 {
                    // Prepare the replacement before closing the only session.
                    self.new_session()?;
                    self.switch_tab(0);
                }
                self.history.finish_session(&self.session.id)?;
                let closed = self.active_tab;
                let next = if order.len() == 1 {
                    1
                } else {
                    order
                        .get(position + 1)
                        .copied()
                        .unwrap_or_else(|| order[position - 1])
                };
                self.switch_tab(next);
                self.tabs.remove(closed);
                if self.active_tab > closed {
                    self.active_tab -= 1;
                }
                self.notice = "Aba fechada. A conversa continua em /sessions.".into();
            }
        }
        Ok(())
    }
    fn any_running(&self) -> bool {
        self.running.is_some() || self.tabs.iter().flatten().any(|tab| tab.running.is_some())
    }
    fn poll_all(&mut self, db: &Db) -> bool {
        if self.metrics_version.is_none()
            || self.metrics_checked.elapsed() >= Duration::from_secs(1)
        {
            self.refresh_execution_metrics(db, self.metrics_version.is_none());
        }
        let was_running = self.running.is_some();
        self.poll(db);
        let mut finished = was_running && self.running.is_none();
        for index in 0..self.tabs.len() {
            if let Some(mut tab) = self.tabs[index].take() {
                self.swap_tab(&mut tab);
                let was_running = self.running.is_some();
                self.poll(db);
                if was_running && self.running.is_none() {
                    self.unread = true;
                    finished = true;
                }
                self.swap_tab(&mut tab);
                self.tabs[index] = Some(tab);
            }
        }
        finished
    }
    fn refresh_execution_metrics(&mut self, db: &Db, force: bool) {
        let version =
            db.0.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
                .ok();
        if force || version != self.metrics_version {
            let hydrate = |turns: &mut [Turn]| {
                for turn in turns {
                    if let Some(id) = turn.execution.as_ref().map(|job| job.id.clone())
                        && let Ok(Some(latest)) = execution(db, &id)
                    {
                        turn.execution = Some(latest);
                    }
                }
            };
            hydrate(&mut self.turns);
            for tab in self.tabs.iter_mut().flatten() {
                hydrate(&mut tab.turns);
            }
        }
        self.metrics_version = version;
        self.metrics_checked = Instant::now();
    }
    fn conversation_stats(&self) -> ConversationStats {
        ConversationStats::from_prompts(
            self.turns
                .iter()
                .enumerate()
                .map(|(index, turn)| {
                    PromptStats::from_execution(
                        index + 1,
                        &turn.prompt,
                        turn.finished,
                        turn.execution.as_ref(),
                    )
                })
                .collect(),
        )
    }
    fn show_usage(&mut self) {
        self.settings_open = false;
        self.agent_view.selected = None;
        self.note = self.conversation_stats().detail_lines();
        self.metrics_open = true;
        self.focus_note = true;
        self.agents_expanded = false;
        self.follow = false;
    }
    fn selected_metric(&self) -> usize {
        self.metric_pick
            .unwrap_or_else(|| self.turns.len().saturating_sub(1))
            .min(self.turns.len().saturating_sub(1))
    }
    fn show_feedback(&mut self, db: &Db) {
        self.settings_open = false;
        self.refresh_execution_metrics(db, true);
        self.stage = Stage::Feedback(FeedbackPicker {
            selected: self.selected_metric(),
            scroll: 0,
        });
        self.notice.clear();
    }
    fn rate_turn(&mut self, index: usize, db: &Db) -> Result<()> {
        self.refresh_execution_metrics(db, true);
        let id = self
            .turns
            .get(index)
            .and_then(|turn| turn.execution.as_ref())
            .filter(|job| job.ended_at.is_some())
            .map(|job| job.id.clone())
            .context("Esta resposta ainda não tem uma execução concluída para avaliar.")?;
        self.metric_pick = Some(index);
        self.edit_form_for(Action::Feedback, Some(&id))
    }
    fn rate_selected_turn(&mut self, db: &Db) -> Result<()> {
        self.rate_turn(self.selected_metric(), db)
    }
    fn widget_action(&mut self, action: WidgetAction, db: &Db) -> Result<()> {
        self.settings_open = false;
        self.refresh_execution_metrics(db, true);
        let selected = self.selected_metric();
        match action {
            WidgetAction::Details => self.show_usage(),
            WidgetAction::Previous => self.metric_pick = Some(selected.saturating_sub(1)),
            WidgetAction::Next => {
                self.metric_pick = Some((selected + 1).min(self.turns.len().saturating_sub(1)))
            }
            WidgetAction::Rate => self.show_feedback(db),
        }
        Ok(())
    }
    fn finish_all(&mut self, db: &Db) -> Result<()> {
        self.poll_all(db);
        if let Some(running) = &mut self.running {
            running.job.cancel();
        }
        for tab in self.tabs.iter_mut().flatten() {
            if let Some(running) = &mut tab.running {
                running.job.cancel();
            }
        }
        let mut result = self.finish(db);
        drop(self.running.take());
        for index in 0..self.tabs.len() {
            if let Some(mut tab) = self.tabs[index].take() {
                self.swap_tab(&mut tab);
                let finished = self.finish(db);
                if result.is_ok() {
                    result = finished;
                }
                drop(self.running.take());
                self.swap_tab(&mut tab);
                self.tabs[index] = Some(tab);
            }
        }
        result
    }
    fn arguments(&self, extra: Vec<OsString>) -> Vec<OsString> {
        let mut args = vec![
            path_option("allow-workspace", &self.cwd),
            path_option("db", &self.options.db),
            path_option("config", &self.options.config),
            path_option("sessions", &self.options.sessions),
            format!("--timezone={}", self.options.timezone).into(),
        ];
        if self.options.no_policy {
            args.push("--no-policy".into());
        }
        args.extend(extra);
        args
    }
    fn navigate(&mut self, page: Page, db: &Db, guard: &mut Option<TerminalGuard>) -> Result<()> {
        ensure!(
            !self.any_running(),
            "Há pedidos em execução nas abas. Aguarde ou cancele antes de abrir esta tela."
        );
        drop(guard.take());
        let result = if page == Page::Setup {
            app_ui::interactive(
                &self.executable,
                &self.arguments(vec!["setup".into()]),
                &self.cwd,
                &self.interrupted,
            )
            .map(|_| ())
        } else {
            app_ui::run(
                db,
                Options {
                    no_policy: self.options.no_policy,
                    cwd: self.cwd.clone(),
                    db: self.options.db.clone(),
                    config: self.options.config.clone(),
                    sessions: self.options.sessions.clone(),
                    timezone: self.options.timezone,
                    page,
                },
            )
        };
        let mut terminal = TerminalGuard::enter_with_mouse()?;
        terminal.set_mouse_capture(!self.selecting_text)?;
        *guard = Some(terminal);
        self.refresh_execution_metrics(db, true);
        self.refresh_profiles();
        if let Some(chosen) = &self.chosen {
            if let Some(fresh) = self
                .catalog
                .choices
                .iter()
                .find(|c| c.name == chosen.name && c.path == chosen.path && c.problem.is_none())
            {
                self.chosen = Some(fresh.clone());
                if self
                    .note
                    .first()
                    .is_some_and(|line| line == "EQUIPE CONFIGURADA")
                {
                    self.note = fresh.team_details();
                    self.focus_note = true;
                    self.follow = false;
                }
            } else {
                self.chosen = None;
                self.stage = Stage::Profiles;
                self.notice =
                    "O perfil mudou ou ficou indisponível. Escolha a equipe novamente.".into();
            }
        }
        if let Err(error) = result {
            self.notice = format!("{error:#}");
        }
        Ok(())
    }
    fn update(&mut self, guard: &mut Option<TerminalGuard>) -> Result<()> {
        ensure!(
            !self.any_running(),
            "Há pedidos em execução nas abas. Aguarde ou cancele antes de atualizar o StackPulse."
        );
        drop(guard.take());
        let result = app_ui::interactive(
            &self.executable,
            &self.arguments(vec!["update".into()]),
            &self.cwd,
            &self.interrupted,
        );
        let mut terminal = TerminalGuard::enter_with_mouse()?;
        terminal.set_mouse_capture(!self.selecting_text)?;
        *guard = Some(terminal);
        let status = result.context("Não foi possível executar a atualização")?;
        ensure!(
            status.success(),
            "A atualização não foi concluída ({status}). Consulte a saída no terminal."
        );
        self.notice = "StackPulse atualizado. Saia com /exit e abra stackpulse novamente para usar a nova versão.".into();
        Ok(())
    }
    fn send(&mut self, prompt: String, preview: bool) -> Result<()> {
        ensure!(
            self.running.is_none(),
            "Aguarde o pedido atual terminar ou use Esc para cancelar."
        );
        let chosen = self.chosen.as_ref().context("Escolha um perfil primeiro")?;
        ensure!(
            !prompt.trim().is_empty(),
            "Escreva o pedido antes de enviar."
        );
        let mut form = self.run_form.clone();
        form.fields[0].value.clone_from(&prompt);
        form.fields[1].value.clone_from(&chosen.name);
        if preview {
            form.fields[5].value = "sim".into();
        }
        let invocation = ui_commands::build(&form)?;
        // build validated this boolean; prompt/benchmark strings can themselves
        // contain flag names and must never change the execution mode.
        let preview = form.fields[5].value.trim().eq_ignore_ascii_case("sim");
        let args = self.arguments(invocation.args);
        // Read durable history before appending this request. /clear only hides
        // the view, and switching profiles must retain the same conversation.
        let context = self.history.context(&self.session.id)?;
        let status = if preview {
            "Prévia · preparando…"
        } else {
            "Trabalhando…"
        };
        let turn_id = self
            .history
            .start_turn(&self.session.id, &prompt, &chosen.name, status)?;
        let started = Job::start_chat(&self.executable, &args, &self.cwd, &context, !preview);
        let job = match started {
            Ok(job) => job,
            Err(error) => {
                let status = format!(
                    "{}Erro ao iniciar: {error:#}",
                    if preview { "Prévia · " } else { "" }
                );
                self.history
                    .update_turn(&self.session.id, &turn_id, &[], &status, None, true)?;
                return Err(error);
            }
        };
        self.activity = ActivitySnapshot::default();
        self.agent_scroll = 0;
        self.agent_view = AgentViewState::default();
        let turn = self.turns.len();
        self.session.updated_at = Utc::now();
        if self.session.turns == 0 && !self.session.custom_title {
            self.session.title =
                widget::clean(&prompt.split_whitespace().collect::<Vec<_>>().join(" "), 60);
        }
        self.session.turns += 1;
        self.turns.push(Turn {
            id: turn_id,
            prompt,
            profile: chosen.name.clone(),
            lines: vec![],
            status: status.into(),
            execution: None,
            finished: false,
        });
        self.metric_pick = None;
        self.metrics_open = false;
        self.running = Some(Running {
            job,
            approval_view: None,
            preview,
            turn,
            emitted: None,
            last_metrics: Instant::now() - Duration::from_secs(2),
            last_history: Instant::now() - Duration::from_secs(2),
        });
        self.composer = Composer::default();
        self.notice.clear();
        self.note.clear();
        self.follow = true;
        self.history_pick = None;
        Ok(())
    }
    fn poll(&mut self, db: &Db) {
        self.poll_agent_receipts();
        let Some(running) = &mut self.running else {
            return;
        };
        if let Some(snapshot) = running.job.activity() {
            self.activity = snapshot;
        }
        let lines = running.job.lines();
        if running.emitted.is_none() {
            running.emitted = emitted_execution_id(&lines);
        }
        let turn = &mut self.turns[running.turn];
        turn.lines = answer_lines(&lines, running.emitted.as_deref());
        turn.status = format!(
            "{}{} · {}",
            if running.preview { "Prévia · " } else { "" },
            if running.job.cancelling() {
                "Cancelando e salvando registro…"
            } else {
                "Trabalhando…"
            },
            widget::duration(running.job.elapsed().as_millis() as i64)
        );
        let result = running.job.poll();
        let done = !matches!(result, Ok(None));
        if done || running.last_metrics.elapsed() >= Duration::from_secs(1) {
            if let Some(id) = &running.emitted {
                turn.execution = execution(db, id).ok().flatten();
            }
            running.last_metrics = Instant::now();
        }
        if !done && running.last_history.elapsed() >= Duration::from_secs(1) {
            if let Err(error) =
                self.history
                    .save_activity(&self.session.id, &turn.id, &self.activity)
            {
                self.notice = format!("Atividade não pôde ser salva: {error:#}");
            }
            if let Err(error) = self.history.update_turn(
                &self.session.id,
                &turn.id,
                &turn.lines,
                &turn.status,
                turn.execution.as_ref(),
                false,
            ) {
                self.notice = format!("Histórico não pôde ser atualizado: {error:#}");
            }
            running.last_history = Instant::now();
        }
        if done {
            if let Some(snapshot) = running.job.activity() {
                self.activity = snapshot;
            }
            let final_status = match &result {
                Ok(Some(result)) if result.cancelled => AgentStatus::Cancelled,
                Ok(Some(result)) if result.success => AgentStatus::Unknown,
                _ => AgentStatus::Failed,
            };
            for agent in &mut self.activity.agents {
                if agent.started_at.is_some() && agent.finished_at.is_none() {
                    agent.finished_at = Some(Utc::now());
                }
                if matches!(
                    agent.status,
                    AgentStatus::Starting | AgentStatus::Running | AgentStatus::Waiting
                ) {
                    agent.status = final_status;
                }
            }
            if let Err(error) =
                self.history
                    .save_activity(&self.session.id, &turn.id, &self.activity)
            {
                self.notice = format!("Atividade não pôde ser salva: {error:#}");
            }
            let lines = running.job.transcript_lines();
            if running.emitted.is_none() {
                running.emitted = emitted_execution_id(&lines);
            }
            turn.lines = answer_lines(&lines, running.emitted.as_deref());
            if let Some(id) = &running.emitted {
                turn.execution = execution(db, id).ok().flatten();
            }
            turn.finished = true;
            turn.status = match result {
                Ok(Some(result)) if result.cancelled => "Cancelado · registro preservado".into(),
                Ok(Some(result)) if result.success => "Concluído".into(),
                Ok(Some(result)) => format!("Falhou · {}", result.message),
                Err(error) => format!("Erro: {error:#}"),
                _ => unreachable!(),
            };
            if running.preview {
                turn.status = format!("Prévia · {}", turn.status);
            }
            if turn.execution.is_some() {
                self.notice =
                    "/feedback para avaliar entrega e rapidez · ou envie outro pedido".into();
            }
            if let Err(error) = self.history.update_turn(
                &self.session.id,
                &turn.id,
                &turn.lines,
                &turn.status,
                turn.execution.as_ref(),
                true,
            ) {
                self.notice = format!("Execução terminou, mas o histórico falhou: {error:#}");
            }
            let response = turn.lines.join("\n");
            let render = !running.preview && crate::chat_preview::has_mermaid(&response);
            if render && let Err(error) = self.render_response(&response) {
                self.notice = format!("Não foi possível renderizar o diagrama: {error:#}");
            }
            if let Err(error) = self.finish_agent_receipts() {
                self.notice = format!("Falha ao finalizar intervenções: {error:#}");
            }
            self.running = None;
            self.refresh_profiles();
            if let Some(chosen) = &self.chosen
                && let Some(fresh) = self
                    .catalog
                    .choices
                    .iter()
                    .find(|c| c.name == chosen.name && c.problem.is_none())
            {
                self.chosen = Some(fresh.clone());
            }
        }
    }
    fn finish(&mut self, db: &Db) -> Result<()> {
        let interventions = self.finish_agent_receipts();
        let saved = (|| -> Result<()> {
            if let Some(running) = &mut self.running {
                running.job.cancel();
                let lines = running.job.transcript_lines();
                if running.emitted.is_none() {
                    running.emitted = emitted_execution_id(&lines);
                }
                let turn = &mut self.turns[running.turn];
                turn.lines = answer_lines(&lines, running.emitted.as_deref());
                if let Some(id) = &running.emitted {
                    turn.execution = execution(db, id).ok().flatten();
                }
                turn.status = "Interrompido · terminal encerrado".into();
                if running.preview {
                    turn.status = format!("Prévia · {}", turn.status);
                }
                turn.finished = true;
                for agent in &mut self.activity.agents {
                    if agent.started_at.is_some() && agent.finished_at.is_none() {
                        agent.finished_at = Some(Utc::now());
                    }
                    if matches!(
                        agent.status,
                        AgentStatus::Starting | AgentStatus::Running | AgentStatus::Waiting
                    ) {
                        agent.status = AgentStatus::Cancelled;
                    }
                }
                self.history
                    .save_activity(&self.session.id, &turn.id, &self.activity)?;
                self.history.update_turn(
                    &self.session.id,
                    &turn.id,
                    &turn.lines,
                    &turn.status,
                    turn.execution.as_ref(),
                    true,
                )?;
            }
            Ok(())
        })();
        let released = self.history.finish_session(&self.session.id);
        saved.and(interventions).and(released)
    }
    fn edit_form(&mut self, action: Action) -> Result<()> {
        self.edit_form_for(action, None)
    }
    fn edit_title(&mut self) {
        let value = self.session.title.clone();
        let mut form = self.run_form.clone();
        form.title = "Título da conversa".into();
        form.fields = vec![ui_commands::Field {
            label: "Título".into(),
            hint: "Até 80 caracteres · o título fica salvo no histórico.".into(),
            value: value.clone(),
            required: true,
            multiline: false,
        }];
        self.agent_view.selected = None;
        self.stage = Stage::Form(ChatForm {
            renaming: true,
            transfer: None,
            form,
            indices: vec![0],
            inputs: vec![Composer {
                cursor: value.len(),
                value,
            }],
            selected: 0,
            error: String::new(),
        });
    }
    fn edit_transfer(&mut self, import: bool) {
        let id = if let Stage::Sessions(picker) = &self.stage {
            picker
                .sessions
                .get(picker.selected)
                .map(|s| s.id.clone())
                .unwrap_or_else(|| self.session.id.clone())
        } else {
            self.session.id.clone()
        };
        let value = if import {
            String::new()
        } else {
            self.cwd
                .join(format!("stackpulse-{id}.jsonl"))
                .to_string_lossy()
                .into_owned()
        };
        let form = Form {
            action: Action::Import,
            title: if import {
                "Importar conversa · JSONL"
            } else {
                "Exportar conversa · JSONL"
            }
            .into(),
            description: "Operação local; não chama providers.".into(),
            fields: vec![ui_commands::Field {
                label: if import {
                    "Arquivo de origem"
                } else {
                    "Salvar como"
                }
                .into(),
                hint: if import {
                    "StackPulse, Codex ou Claude · cole o caminho · ~/ e espaços são aceitos."
                } else {
                    "Escolha um novo arquivo .jsonl; arquivos existentes são preservados."
                }
                .into(),
                value: value.clone(),
                required: true,
                multiline: false,
            }],
        };
        self.stage = Stage::Form(ChatForm {
            renaming: false,
            transfer: Some(if import {
                Transfer::Import
            } else {
                Transfer::Export(id)
            }),
            form,
            indices: vec![0],
            inputs: vec![Composer {
                cursor: value.len(),
                value,
            }],
            selected: 0,
            error: String::new(),
        });
    }

    fn edit_form_for(&mut self, action: Action, selected_id: Option<&str>) -> Result<()> {
        let (mut form, indices) = if action == Action::Run {
            (self.run_form.clone(), vec![2, 3, 4, 5])
        } else {
            let job = self.turns.iter().rev().filter_map(|t| t.execution.as_ref()).find(|job| selected_id.is_none_or(|id| job.id == id)).context("Ainda não há uma execução nesta conversa para avaliar. /history abre o histórico.")?;
            let mut form = ui_commands::form(Action::Feedback, Some(&job.id));
            if let Some((index, turn)) = self.turns.iter().enumerate().find(|(_, turn)| {
                turn.execution
                    .as_ref()
                    .is_some_and(|execution| execution.id == job.id)
            }) {
                form.title = format!(
                    "Avaliar pedido #{} · {}",
                    index + 1,
                    widget::clean(
                        &turn.prompt.split_whitespace().collect::<Vec<_>>().join(" "),
                        60
                    )
                );
            }
            if let Some(feedback) = &job.feedback {
                form.fields[1].value = feedback.delivered.to_string();
                form.fields[2].value = feedback.speed.to_string();
                form.fields[3].value.clone_from(&feedback.note);
            }
            (form, vec![1, 2, 3])
        };
        if action == Action::Run {
            form.title = "Opções dos próximos pedidos".into();
        }
        let inputs = indices
            .iter()
            .map(|i| {
                let value = form.fields[*i].value.clone();
                Composer {
                    cursor: value.len(),
                    value,
                }
            })
            .collect();
        self.stage = Stage::Form(ChatForm {
            renaming: false,
            transfer: None,
            form,
            indices,
            inputs,
            selected: 0,
            error: String::new(),
        });
        Ok(())
    }
    fn save_form(&mut self, db: &mut Db) -> Result<()> {
        let Stage::Form(editor) = &mut self.stage else {
            return Ok(());
        };
        if let Some(transfer) = editor.transfer.clone() {
            let value = editor.inputs[0].value.trim();
            ensure!(!value.is_empty(), "Informe o caminho do arquivo JSONL.");
            let path = if let Some(rest) = value.strip_prefix("~/") {
                PathBuf::from(std::env::var_os("HOME").context("Pasta do usuário indisponível")?)
                    .join(rest)
            } else {
                self.cwd.join(value)
            };
            match transfer {
                Transfer::Import => {
                    let result = self.history.import_session(&path)?;
                    let saved = self.history.read_session(&result.id)?;
                    // Restore execution snapshots when the metrics cache is new.
                    for turn in &saved.turns {
                        if let Some(job) = &turn.execution
                            && execution(db, &job.id)?.is_none()
                        {
                            db.save_execution(job)?;
                        }
                    }
                    self.show_sessions()?;
                    if let Stage::Sessions(picker) = &mut self.stage {
                        picker.selected = picker
                            .sessions
                            .iter()
                            .position(|s| s.id == result.id)
                            .unwrap_or_default();
                    }
                    self.notice = if result.duplicate {
                        "Essa sessão já foi importada; nenhuma conversa duplicada.".into()
                    } else {
                        format!(
                            "{} {} · {} · histórico para consulta; nenhuma execução iniciada.",
                            result.turns,
                            if result.turns == 1 {
                                "pedido importado"
                            } else {
                                "pedidos importados"
                            },
                            result.client
                        )
                    };
                }
                Transfer::Export(id) => {
                    self.history.export_session(&id, &path)?;
                    self.show_sessions()?;
                    self.notice = format!("Conversa exportada: {}", path.display());
                }
            }
            return Ok(());
        }
        if editor.renaming {
            let title = self.history.rename_session(
                &self.session.id,
                &self.cwd,
                &editor.inputs[0].value,
            )?;
            self.session.title = title;
            self.session.custom_title = true;
            self.stage = if self.chosen.is_some() {
                Stage::Chat
            } else {
                Stage::Profiles
            };
            self.notice = "Título salvo nesta conversa.".into();
            return Ok(());
        }
        for (i, input) in editor.indices.iter().zip(&editor.inputs) {
            editor.form.fields[*i].value.clone_from(&input.value);
        }
        if editor.form.action == Action::Run {
            editor.form.fields[0].value = "Validar opções".into();
        }
        ui_commands::build(&editor.form)?;
        if editor.form.action == Action::Run {
            self.run_form = editor.form.clone();
            self.run_form.fields[0].value.clear();
            self.notice = "Opções aplicadas aos próximos pedidos.".into();
        } else {
            let id = editor.form.fields[0].value.clone();
            workflow::save_feedback(
                db,
                &id,
                Feedback {
                    delivered: editor.form.fields[1]
                        .value
                        .trim()
                        .replace(',', ".")
                        .parse()?,
                    speed: editor.form.fields[2].value.trim().parse()?,
                    note: editor.form.fields[3].value.clone(),
                    recorded_at: Utc::now(),
                },
            )?;
            for turn in &mut self.turns {
                if turn.execution.as_ref().is_some_and(|e| e.id == id) {
                    turn.execution = execution(db, &id)?;
                }
            }
            if let Some(turn) = self
                .turns
                .iter()
                .find(|turn| turn.execution.as_ref().is_some_and(|item| item.id == id))
            {
                self.history.update_turn(
                    &self.session.id,
                    &turn.id,
                    &turn.lines,
                    &turn.status,
                    turn.execution.as_ref(),
                    turn.finished,
                )?;
            }
            self.notice = "Feedback salvo junto do consumo desta execução.".into();
        }
        self.stage = Stage::Chat;
        Ok(())
    }
    fn render_response(&mut self, response: &str) -> Result<()> {
        let path = crate::chat_preview::write_preview(response)?;
        let status = match (self.browser_opener)(&path) {
            Ok(()) => "Abertura automática solicitada ao navegador",
            Err(_) => "Não foi possível abrir o navegador; abra o arquivo manualmente",
        };
        self.note.extend([
            format!("{status}: {}", path.display()),
            "Mermaid detectado · Markdown e diagramas renderizados no navegador. Requer internet para carregar as bibliotecas de renderização.".into(),
        ]);
        self.notice =
            "Mermaid detectado. O caminho da visualização está no final da conversa.".into();
        Ok(())
    }
    fn suggestions(&self) -> Vec<(&'static str, &'static str)> {
        if !self.composer.value.starts_with('/')
            || self.composer.value.contains(char::is_whitespace)
        {
            return vec![];
        }
        COMMANDS
            .iter()
            .copied()
            .filter(|(c, _)| c.starts_with(&self.composer.value))
            .collect()
    }
    fn command(&mut self, db: &mut Db, guard: &mut Option<TerminalGuard>) -> Result<bool> {
        let input = self.composer.value.clone();
        let trimmed = input.trim();
        if !trimmed.starts_with('/') {
            self.send(input, false)?;
            return Ok(false);
        }
        let (cmd, args) = trimmed
            .split_once(char::is_whitespace)
            .unwrap_or((trimmed, ""));
        let page = match cmd {
            "/menu" => Some(Page::Overview),
            "/history" => Some(Page::Executions),
            "/trend" => Some(Page::Trend),
            "/report" => Some(Page::Report),
            "/setup" => Some(Page::Setup),
            "/widget" => Some(Page::Widget),
            _ => None,
        };
        ensure!(
            matches!(
                cmd,
                "/preview" | "/skip-dangerous" | "/no-policy" | "/dangerously-skip-permissions"
            ) || args.trim().is_empty(),
            "Esse comando não aceita argumentos."
        );
        if let Some(page) = page {
            self.composer = Composer::default();
            self.navigate(page, db, guard)?;
            return Ok(false);
        }
        self.focus_note = false;
        self.metrics_open = false;
        match cmd {
            "/skip-dangerous" | "/no-policy" | "/dangerously-skip-permissions" => {
                let enabled = match args.trim() {
                    "" | "on" => true,
                    "off" => false,
                    _ => anyhow::bail!("Use /skip-dangerous [on|off]."),
                };
                self.options.no_policy = enabled;
                self.notice = if enabled {
                    "Modo sem sandbox/aprovações ativado para os próximos pedidos Codex/Grok em todas as abas."
                } else {
                    "Modo sem sandbox/aprovações desativado; próximos pedidos seguem as permissões configuradas."
                }.into();
                self.note.push(self.notice.clone());
                self.note.push(
                    "Execuções já iniciadas não mudam. A opção não é salva ao fechar o aplicativo."
                        .into(),
                );
                self.follow = true;
            }
            "/update" => self.update(guard)?,
            "/title" => self.edit_title(),
            "/import-session" => self.edit_transfer(true),
            "/export-session" => self.edit_transfer(false),
            "/settings" => self.settings_open = true,
            "/agents" => self.agents_expanded = !self.agents_expanded,
            "/team" => {
                let name = self
                    .chosen
                    .as_ref()
                    .context("Escolha um perfil primeiro")?
                    .name
                    .clone();
                self.refresh_profiles();
                let fresh = self.catalog.choices.iter().find(|choice| choice.name == name)
                    .context("O perfil ativo ficou indisponível. Use /profile para escolher a equipe novamente.")?
                    .clone();
                self.note = fresh.team_details();
                self.focus_note = true;
                self.chosen = Some(fresh);
                self.agents_expanded = false;
                self.scroll = 0;
                self.follow = false;
            }
            "/profile" | "/profiles" => {
                self.refresh_profiles();
                self.stage = Stage::Profiles;
            }
            "/sessions" => self.show_sessions()?,
            "/new" => {
                self.composer = Composer::default();
                self.new_session()?;
            }
            "/options" => self.edit_form(Action::Run)?,
            "/feedback" => self.rate_selected_turn(db)?,
            "/usage" => {
                self.refresh_execution_metrics(db, true);
                self.show_usage();
            }
            "/credits" => {
                self.note = [
                    "OPCIONAIS E CRÉDITOS",
                    "Memória de longo prazo: AI-Memory",
                    "Criado por Fabio Akita (AkitaOnRails).",
                    "Projeto: https://github.com/akitaonrails/ai-memory",
                    "Criador: https://akitaonrails.com",
                    "Instalação opcional disponível no setup.",
                    "Conexão aos agentes ainda não configurada pelo StackPulse.",
                    "Consumo no terminal: AI-UsageBar (CLI/TUI)",
                    "Projeto: https://github.com/akitaonrails/ai-usagebar",
                    "Também criado por Fabio Akita (AkitaOnRails).",
                    "Opcional desmarcado; disponível no setup em plataforma compatível.",
                    "Barra gráfica configurada separadamente.",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect();
                self.follow = true;
            }
            "/help" => {
                self.note = COMMANDS
                    .iter()
                    .map(|(c, d)| format!("{c:<12} {d}"))
                    .collect();
                self.note.push(
                    "Enter envia · Alt+Enter / Ctrl+J nova linha · PgUp/PgDn histórico · Esc cancela".into(),
                );
                self.note.push("Abas: Ctrl+N nova · Alt+←/→ ou F6 alterna · Ctrl+W fecha · ● trabalhando · • conclusão não vista".into());
                self.note.push("F8 seleciona texto: arraste e use Copiar do terminal; F8/Esc volta. A tela fica parada enquanto os pedidos continuam. Fora desse modo, clique e roda navegam.".into());
                self.note.push("Cada pedido inicia uma execução independente. /sessions reabre conversas deste projeto sem reenviar seu conteúdo ao provider.".into());
                self.follow = true;
            }
            "/clear" => {
                self.hidden_turns = self.turns.len();
                self.activity = ActivitySnapshot::default();
                self.agent_scroll = 0;
                self.note.clear();
                self.scroll = 0;
                self.follow = true;
                self.history_pick = None;
                self.notice = "Tela limpa. A conversa foi preservada e pode ser reaberta em /sessions; /history mostra métricas.".into();
            }
            "/preview" => self.send(args.into(), true)?,
            "/exit" => return Ok(true),
            _ => anyhow::bail!("Comando desconhecido. Use /help ou Tab para completar."),
        }
        self.composer = Composer::default();
        Ok(false)
    }
    /// Decisions apply only to the exact request the user has already seen.
    fn approval_key(&mut self, key: KeyEvent) -> bool {
        let Some(running) = &mut self.running else {
            return false;
        };
        let pending = running.job.pending_approvals();
        if pending.is_empty() {
            running.approval_view = None;
            return false;
        }
        // Keep tab navigation available while one job waits for permission.
        if key.code == KeyCode::F(6)
            || (key.modifiers.contains(KeyModifiers::ALT)
                && matches!(
                    key.code,
                    KeyCode::Left | KeyCode::Right | KeyCode::Char('1'..='9')
                ))
            || (key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Tab | KeyCode::BackTab))
        {
            return false;
        }
        let ctrl = key.modifiers == KeyModifiers::CONTROL;
        if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
            running.job.cancel();
            return true;
        }
        let Some(view) = &mut running.approval_view else {
            return true;
        };
        match key.code {
            KeyCode::Up => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::Down => view.scroll = view.scroll.saturating_add(1),
            KeyCode::PageUp => view.scroll = view.scroll.saturating_sub(8),
            KeyCode::PageDown => view.scroll = view.scroll.saturating_add(8),
            KeyCode::Home => view.scroll = 0,
            KeyCode::End => view.scroll = usize::MAX,
            KeyCode::Char(c @ ('y' | 'n')) if ctrl && key.kind == KeyEventKind::Press => {
                if view.visible
                    && pending.iter().any(|request| {
                        request.request_id == view.request.request_id
                            && request.method == view.request.method
                            && request.params == view.request.params
                    })
                {
                    let request = view.request.clone();
                    match running.job.decide_approval(&request, c == 'y') {
                        Ok(()) => {
                            running.approval_view = None;
                            self.notice = if c == 'y' {
                                "Aprovação enviada para esta solicitação."
                            } else {
                                "Solicitação negada."
                            }
                            .into();
                        }
                        Err(error) => self.notice = format!("{error:#}"),
                    }
                }
            }
            _ => {}
        }
        true
    }

    fn draw_approval(&mut self, frame: &mut Frame) -> bool {
        let Some(running) = &mut self.running else {
            return false;
        };
        let requests = running.job.pending_approvals();
        let count = requests.len();
        let Some(request) = requests.into_iter().next() else {
            running.approval_view = None;
            return false;
        };
        if !running.approval_view.as_ref().is_some_and(|view| {
            view.request.request_id == request.request_id
                && view.request.method == request.method
                && view.request.params == request.params
        }) {
            running.approval_view = Some(ApprovalView {
                request,
                scroll: 0,
                visible: false,
            });
        }
        let view = running.approval_view.as_mut().unwrap();
        view.visible = true;
        let width = frame.width.saturating_sub(4);
        let capacity = frame.height.saturating_sub(10);
        let params = serde_json::to_string_pretty(&view.request.params).unwrap_or_default();
        let details = format!(
            "Solicitação: {}\nMétodo: {}\nParâmetros completos (comando, motivo, diretório, thread e alterações):\n{}",
            view.request.request_id, view.request.method, params
        );
        let lines = wrap(&details, width);
        view.scroll = view.scroll.min(lines.len().saturating_sub(capacity));
        put(
            frame,
            2,
            1,
            "STACKPULSE · PERMISSÃO NECESSÁRIA",
            Tone::Warning,
            width,
        );
        put(
            frame,
            2,
            2,
            &format!("Conversa: {} · {count} pendente(s)", self.session.title),
            Tone::Text,
            width,
        );
        let scope = if view.request.method == "item/permissions/requestApproval" {
            "Ctrl+Y aprovar nesta rodada · Ctrl+N negar"
        } else {
            "Ctrl+Y aprovar uma vez · Ctrl+N negar"
        };
        put(frame, 2, 4, scope, Tone::Accent, width);
        for (offset, line) in lines.iter().skip(view.scroll).take(capacity).enumerate() {
            put(frame, 2, 6 + offset, line, Tone::Text, width);
        }
        put(
            frame,
            2,
            frame.height - 3,
            &self.notice,
            Tone::Warning,
            width,
        );
        put(
            frame,
            2,
            frame.height - 2,
            &format!(
                "↑↓/PgUp/PgDn rolar ({}/{}) · Esc cancelar",
                view.scroll + 1,
                lines.len()
            ),
            Tone::Muted,
            width,
        );
        put(
            frame,
            2,
            frame.height - 1,
            if self.selecting_text {
                "COPIAR · F8/Esc voltar antes de decidir"
            } else {
                "Alt+←/→ mudar aba · Enter não confirma"
            },
            Tone::Muted,
            width,
        );
        true
    }

    fn key(
        &mut self,
        key: KeyEvent,
        db: &mut Db,
        guard: &mut Option<TerminalGuard>,
    ) -> Result<bool> {
        if key.kind == KeyEventKind::Release {
            return Ok(false);
        }
        if key.code == KeyCode::F(8) || (self.selecting_text && key.code == KeyCode::Esc) {
            if key.kind == KeyEventKind::Repeat {
                return Ok(false);
            }
            let selecting = !self.selecting_text;
            if let Some(guard) = guard {
                guard.set_mouse_capture(!selecting)?;
            }
            self.selecting_text = selecting;
            return Ok(false);
        }
        // Native copy shortcuts must never edit or submit the draft. Leave
        // selection mode explicitly before interacting with the application.
        if self.selecting_text {
            return Ok(false);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if self.approval_key(key) {
            return Ok(false);
        }
        let tab_action = match key.code {
            KeyCode::F(2) if matches!(self.stage, Stage::Chat) => Some(TabAction::Rename),
            KeyCode::Char('n') if ctrl => Some(TabAction::New),
            KeyCode::Char('w') if ctrl => Some(TabAction::Close),
            KeyCode::Left if alt => Some(TabAction::Previous),
            KeyCode::Right if alt => Some(TabAction::Next),
            KeyCode::BackTab if ctrl => Some(TabAction::Previous),
            KeyCode::Tab if ctrl => Some(TabAction::Next),
            KeyCode::F(6) if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Some(TabAction::Previous)
            }
            KeyCode::F(6) => Some(TabAction::Next),
            KeyCode::Char(c @ '1'..='9') if alt => self
                .tab_order()
                .get(c as usize - '1' as usize)
                .copied()
                .map(TabAction::Select),
            _ => None,
        };
        if let Some(action) = tab_action {
            if let Err(error) = self.tab_action(action) {
                self.notice = format!("{error:#}");
            }
            return Ok(false);
        }
        if self.settings_open {
            match key.code {
                KeyCode::Esc => self.settings_open = false,
                KeyCode::Char('c') if ctrl => self.settings_open = false,
                KeyCode::Up | KeyCode::BackTab => {
                    self.settings_pick = self.settings_pick.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Tab => {
                    self.settings_pick = (self.settings_pick + 1).min(SETTINGS.len() - 1)
                }
                KeyCode::Enter => {
                    if let Err(error) = self.setting(self.settings_pick, db, guard) {
                        self.notice = format!("{error:#}");
                    }
                }
                _ => {}
            }
            return Ok(false);
        }
        if matches!(self.stage, Stage::Chat) && self.agent_view.selected.is_some() {
            if key.code == KeyCode::Esc {
                self.close_agent();
                return Ok(false);
            }
            if key.code == KeyCode::F(4) {
                self.agent_view.selected = None;
                self.agents_expanded = true;
                return Ok(false);
            }
            if self.agent_view.focused {
                match key.code {
                    KeyCode::PageUp => {
                        self.agent_view.scroll = self.agent_view.scroll.saturating_sub(5)
                    }
                    KeyCode::PageDown => {
                        self.agent_view.scroll = self.agent_view.scroll.saturating_add(5)
                    }
                    _ if is_newline(key) => {
                        let id = self.agent_view.selected.clone().unwrap();
                        self.notice = self
                            .agent_view
                            .drafts
                            .entry(id)
                            .or_default()
                            .insert("\n")
                            .err()
                            .unwrap_or_default();
                    }
                    KeyCode::Enter | KeyCode::F(5) => {
                        if let Err(error) = self.send_agent(false) {
                            self.notice = format!("{error:#}");
                        }
                    }
                    KeyCode::F(9) => {
                        if let Err(error) = self.send_agent(true) {
                            self.notice = format!("{error:#}");
                        }
                    }
                    KeyCode::Char('c') if ctrl => {
                        let id = self.agent_view.selected.clone().unwrap();
                        self.agent_view.drafts.insert(id, Composer::default());
                    }
                    _ => {
                        let id = self.agent_view.selected.clone().unwrap();
                        self.notice = self
                            .agent_view
                            .drafts
                            .entry(id)
                            .or_default()
                            .key(key)
                            .err()
                            .unwrap_or_default();
                    }
                }
                return Ok(false);
            }
        }
        if matches!(self.stage, Stage::Chat) {
            match key.code {
                KeyCode::F(7) => {
                    self.widget_action(WidgetAction::Details, db)?;
                    return Ok(false);
                }
                KeyCode::F(4) => {
                    self.agents_expanded = !self.agents_expanded;
                    return Ok(false);
                }
                KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
                    self.agent_scroll = self.agent_scroll.saturating_sub(1);
                    return Ok(false);
                }
                KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                    self.agent_scroll = self.agent_scroll.saturating_add(1);
                    return Ok(false);
                }
                KeyCode::Esc if self.agents_expanded => {
                    self.agents_expanded = false;
                    return Ok(false);
                }
                _ => {}
            }
        }
        if let Some(running) = &mut self.running
            && matches!(self.stage, Stage::Chat)
        {
            match key.code {
                KeyCode::Esc => running.job.cancel(),
                KeyCode::Char('c') if ctrl => running.job.cancel(),
                KeyCode::PageUp => {
                    self.follow = false;
                    self.scroll = self.scroll.saturating_sub(8);
                }
                KeyCode::PageDown => {
                    self.follow = false;
                    self.scroll = self.scroll.saturating_add(8);
                }
                KeyCode::End => self.follow = true,
                _ => {}
            }
            return Ok(false);
        }
        if ctrl && key.code == KeyCode::Char('c') {
            if matches!(&self.stage, Stage::Form(editor) if editor.renaming || editor.transfer.is_some())
            {
                self.stage = if self.chosen.is_some() {
                    Stage::Chat
                } else {
                    Stage::Profiles
                };
                return Ok(false);
            }
            if matches!(self.stage, Stage::Chat) && !self.composer.value.is_empty() {
                self.composer = Composer::default();
                return Ok(false);
            }
            return Ok(true);
        }
        match &mut self.stage {
            Stage::Profiles => match key.code {
                KeyCode::Enter => self.choose(),
                KeyCode::Esc => {
                    if self.chosen.is_some() {
                        self.stage = Stage::Chat;
                    } else {
                        return Ok(true);
                    }
                }
                KeyCode::Up => self.pick = self.pick.saturating_sub(1),
                KeyCode::Down => {
                    self.pick = (self.pick + 1).min(self.choices().len().saturating_sub(1))
                }
                KeyCode::F(2) | KeyCode::F(3) => {
                    if let Err(error) = self.navigate(
                        if key.code == KeyCode::F(2) {
                            Page::Setup
                        } else {
                            Page::Profiles
                        },
                        db,
                        guard,
                    ) {
                        self.notice = format!("{error:#}");
                    }
                }
                KeyCode::Char('r') if ctrl => self.refresh_profiles(),
                _ => {
                    self.notice = self.search.key(key).err().unwrap_or_default();
                    self.pick = 0;
                }
            },
            Stage::Sessions(picker) => match key.code {
                KeyCode::Char('i') => self.edit_transfer(true),
                KeyCode::Char('e') => self.edit_transfer(false),
                KeyCode::Enter => {
                    let id = picker
                        .sessions
                        .get(picker.selected)
                        .map(|session| session.id.clone());
                    if let Some(id) = id
                        && let Err(error) = self.load_session(&id)
                    {
                        self.notice = format!("{error:#}");
                    }
                }
                KeyCode::Esc => self.stage = Stage::Chat,
                KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
                KeyCode::Down => {
                    picker.selected =
                        (picker.selected + 1).min(picker.sessions.len().saturating_sub(1));
                }
                _ => {}
            },
            Stage::Feedback(picker) => match key.code {
                KeyCode::Enter => {
                    let selected = picker.selected;
                    if let Err(error) = self.rate_turn(selected, db) {
                        self.notice = format!("{error:#}");
                    }
                }
                KeyCode::Esc => self.stage = Stage::Chat,
                KeyCode::Up => {
                    picker.selected = picker.selected.saturating_sub(1);
                    picker.scroll = 0;
                    self.notice.clear();
                }
                KeyCode::Down => {
                    picker.selected = (picker.selected + 1).min(self.turns.len().saturating_sub(1));
                    picker.scroll = 0;
                    self.notice.clear();
                }
                KeyCode::PageUp => picker.scroll = picker.scroll.saturating_sub(5),
                KeyCode::PageDown => picker.scroll = picker.scroll.saturating_add(5),
                KeyCode::Home => picker.scroll = 0,
                KeyCode::End => picker.scroll = usize::MAX,
                _ => {}
            },
            Stage::Form(editor) => match key.code {
                KeyCode::Esc => {
                    self.stage = if self.chosen.is_some() {
                        Stage::Chat
                    } else {
                        Stage::Profiles
                    }
                }
                KeyCode::F(5) => {
                    if let Err(error) = self.save_form(db)
                        && let Stage::Form(e) = &mut self.stage
                    {
                        e.error = format!("{error:#}");
                    }
                }
                _ if is_newline(key) => {
                    if editor.selected < editor.inputs.len()
                        && editor.form.fields[editor.indices[editor.selected]].multiline
                    {
                        editor.error = editor.inputs[editor.selected]
                            .insert("\n")
                            .err()
                            .unwrap_or_default();
                    }
                }
                KeyCode::Enter if editor.selected == editor.inputs.len() => {
                    if let Err(error) = self.save_form(db)
                        && let Stage::Form(e) = &mut self.stage
                    {
                        e.error = format!("{error:#}");
                    }
                }
                KeyCode::Enter | KeyCode::Tab | KeyCode::Down => {
                    editor.selected = (editor.selected + 1).min(editor.inputs.len())
                }
                KeyCode::Up | KeyCode::BackTab => {
                    editor.selected = editor.selected.saturating_sub(1)
                }
                _ => {
                    if let Some(input) = editor.inputs.get_mut(editor.selected) {
                        editor.error = input.key(key).err().unwrap_or_default();
                    }
                }
            },
            Stage::Chat => match key.code {
                _ if is_newline(key) => {
                    self.notice = self.composer.insert("\n").err().unwrap_or_default();
                }
                KeyCode::Enter => match self.command(db, guard) {
                    Ok(true) => return Ok(true),
                    Ok(false) => {}
                    Err(error) => self.notice = format!("{error:#}"),
                },
                KeyCode::Tab => {
                    let matches = self.suggestions();
                    if let Some((command, _)) =
                        matches.get(self.command_pick.min(matches.len().saturating_sub(1)))
                    {
                        self.composer.value = command.to_string();
                        self.composer.cursor = self.composer.value.len();
                        self.command_pick = 0;
                    }
                }
                KeyCode::Up if !self.suggestions().is_empty() => {
                    self.command_pick = self.command_pick.saturating_sub(1)
                }
                KeyCode::Down if !self.suggestions().is_empty() => {
                    self.command_pick = (self.command_pick + 1).min(self.suggestions().len() - 1)
                }
                KeyCode::Up if self.composer.value.is_empty() || self.history_pick.is_some() => {
                    if !self.turns.is_empty() {
                        let i = self
                            .history_pick
                            .map_or(self.turns.len() - 1, |i| i.saturating_sub(1));
                        self.history_pick = Some(i);
                        self.composer.value = self.turns[i].prompt.clone();
                        self.composer.cursor = self.composer.value.len();
                    }
                }
                KeyCode::Down if self.history_pick.is_some() => {
                    let i = self.history_pick.unwrap() + 1;
                    if i >= self.turns.len() {
                        self.history_pick = None;
                        self.composer = Composer::default();
                    } else {
                        self.history_pick = Some(i);
                        self.composer.value = self.turns[i].prompt.clone();
                        self.composer.cursor = self.composer.value.len();
                    }
                }
                KeyCode::PageUp => {
                    self.follow = false;
                    self.scroll = self.scroll.saturating_sub(8);
                }
                KeyCode::PageDown => {
                    self.follow = false;
                    self.scroll = self.scroll.saturating_add(8);
                }
                KeyCode::Esc => {
                    self.composer = Composer::default();
                    self.note.clear();
                    self.metrics_open = false;
                    self.notice.clear();
                }
                _ => {
                    self.notice = self.composer.key(key).err().unwrap_or_default();
                    self.command_pick = 0;
                    self.history_pick = None;
                }
            },
        }
        Ok(false)
    }
    fn mouse(
        &mut self,
        event: MouseEvent,
        db: &mut Db,
        guard: &mut Option<TerminalGuard>,
    ) -> Result<bool> {
        if let Some(running) = &mut self.running
            && !running.job.pending_approvals().is_empty()
        {
            if let Some(view) = &mut running.approval_view {
                match event.kind {
                    MouseEventKind::ScrollUp => view.scroll = view.scroll.saturating_sub(3),
                    MouseEventKind::ScrollDown => view.scroll = view.scroll.saturating_add(3),
                    _ => {}
                }
            }
            return Ok(false);
        }
        let x = usize::from(event.column);
        let y = usize::from(event.row);
        let target = self
            .clicks
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(x, y))
            .cloned();
        if matches!(
            event.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            let up = event.kind == MouseEventKind::ScrollUp;
            if self.settings_open {
                self.settings_pick = if up {
                    self.settings_pick.saturating_sub(1)
                } else {
                    (self.settings_pick + 1).min(SETTINGS.len() - 1)
                };
                return Ok(false);
            }
            match &mut self.stage {
                Stage::Profiles => {
                    self.pick = if up {
                        self.pick.saturating_sub(1)
                    } else {
                        (self.pick + 1).min(self.choices().len().saturating_sub(1))
                    }
                }
                Stage::Sessions(picker) => {
                    picker.selected = if up {
                        picker.selected.saturating_sub(1)
                    } else {
                        (picker.selected + 1).min(picker.sessions.len().saturating_sub(1))
                    }
                }
                Stage::Feedback(picker) => match target {
                    Some((_, ClickAction::FeedbackContent)) => {
                        picker.scroll = if up {
                            picker.scroll.saturating_sub(3)
                        } else {
                            picker.scroll.saturating_add(3)
                        };
                    }
                    _ => {
                        picker.selected = if up {
                            picker.selected.saturating_sub(1)
                        } else {
                            (picker.selected + 1).min(self.turns.len().saturating_sub(1))
                        };
                        picker.scroll = 0;
                    }
                },
                Stage::Form(editor) => {
                    editor.selected = if up {
                        editor.selected.saturating_sub(1)
                    } else {
                        (editor.selected + 1).min(editor.inputs.len())
                    }
                }
                Stage::Chat => match target {
                    Some((_, ClickAction::Tab(_))) => {
                        self.tab_action(if up {
                            TabAction::Previous
                        } else {
                            TabAction::Next
                        })?;
                    }
                    Some((_, ClickAction::Agents | ClickAction::Agent(_))) => {
                        self.agent_scroll = if up {
                            self.agent_scroll.saturating_sub(1)
                        } else {
                            self.agent_scroll.saturating_add(1)
                        };
                    }
                    Some((_, ClickAction::AgentDetail(_))) => {
                        self.agent_view.scroll = if up {
                            self.agent_view.scroll.saturating_sub(3)
                        } else {
                            self.agent_view.scroll.saturating_add(3)
                        };
                    }
                    Some((_, ClickAction::Transcript)) => {
                        self.follow = false;
                        self.scroll = if up {
                            self.scroll.saturating_sub(3)
                        } else {
                            self.scroll.saturating_add(3)
                        };
                    }
                    _ => {}
                },
            }
            return Ok(false);
        }
        if event.kind != MouseEventKind::Down(MouseButton::Left) {
            return Ok(false);
        }
        let Some((rect, action)) = target else {
            return Ok(false);
        };
        match action {
            ClickAction::Tab(action) => self.tab_action(action)?,
            ClickAction::Setting(index) => self.setting(index, db, guard)?,
            ClickAction::Agent(id) => self.open_agent(id),
            ClickAction::AgentDetail(action) => match action {
                DetailAction::Back => self.close_agent(),
                DetailAction::Send => self.send_agent(false)?,
                DetailAction::InterruptRedirect => self.send_agent(true)?,
                DetailAction::Composer { first, width } => {
                    if let Some(id) = self.agent_view.selected.clone() {
                        self.agent_view.focused = true;
                        self.agent_view.drafts.entry(id).or_default().place_cursor(
                            x - rect.x,
                            y - rect.y + first,
                            width,
                        );
                    }
                }
                DetailAction::Content => self.agent_view.focused = true,
            },
            ClickAction::Widget(action) => self.widget_action(action, db)?,
            ClickAction::Profile(index) => {
                self.pick = index;
                self.choose();
            }
            ClickAction::Session(id) => self.load_session(&id)?,
            ClickAction::ImportSession => self.edit_transfer(true),
            ClickAction::ExportSession => self.edit_transfer(false),
            ClickAction::Feedback(index) => {
                if let Stage::Feedback(picker) = &mut self.stage {
                    picker.selected = index.min(self.turns.len().saturating_sub(1));
                    picker.scroll = 0;
                    self.notice.clear();
                }
            }
            ClickAction::FeedbackSubmit => {
                if let Stage::Feedback(picker) = &self.stage {
                    let selected = picker.selected;
                    if let Err(error) = self.rate_turn(selected, db) {
                        self.notice = format!("{error:#}");
                    }
                }
            }
            ClickAction::FormField(index, row) => {
                if let Stage::Form(editor) = &mut self.stage {
                    editor.selected = index;
                    editor.inputs[index].place_cursor(
                        x.saturating_sub(4),
                        row,
                        rect.width.saturating_sub(4),
                    );
                }
            }
            ClickAction::Save => {
                if let Err(error) = self.save_form(db) {
                    if let Stage::Form(editor) = &mut self.stage {
                        editor.error = format!("{error:#}");
                    } else {
                        return Err(error);
                    }
                }
            }
            ClickAction::Suggestion(cmd) if self.running.is_none() => {
                self.composer.value = cmd.into();
                self.composer.cursor = cmd.len();
                self.command_pick = 0;
            }
            ClickAction::Composer { first, width } if self.running.is_none() => {
                self.agent_view.focused = false;
                self.composer
                    .place_cursor(x.saturating_sub(rect.x), y - rect.y + first, width);
                self.history_pick = None;
            }
            ClickAction::Submit if self.running.is_none() => {
                self.agent_view.focused = false;
                return self.command(db, guard);
            }
            ClickAction::Cancel => {
                if let Some(running) = &mut self.running {
                    running.job.cancel();
                }
            }
            ClickAction::Sessions => self.show_sessions()?,
            ClickAction::Agents => self.agents_expanded = !self.agents_expanded,
            ClickAction::Search(row) => {
                self.search
                    .place_cursor(x.saturating_sub(4), row, rect.width.saturating_sub(4))
            }
            ClickAction::Back => {
                self.stage = if self.chosen.is_some() {
                    Stage::Chat
                } else {
                    Stage::Profiles
                }
            }
            _ => {}
        }
        Ok(false)
    }
    fn paste(&mut self, value: &str) {
        if self
            .running
            .as_ref()
            .is_some_and(|r| !r.job.pending_approvals().is_empty())
        {
            return;
        }
        if self.selecting_text {
            return;
        }
        if self.settings_open {
            return;
        }
        if matches!(self.stage, Stage::Chat)
            && self.agent_view.focused
            && let Some(id) = self.agent_view.selected.clone()
        {
            self.notice = self
                .agent_view
                .drafts
                .entry(id)
                .or_default()
                .insert(value)
                .err()
                .unwrap_or_default();
            return;
        }
        if self.running.is_some()
            && !matches!(&self.stage, Stage::Form(editor) if editor.renaming || editor.transfer.is_some())
        {
            return;
        }
        match &mut self.stage {
            Stage::Profiles => {
                self.notice = self
                    .search
                    .insert(&value.replace(['\n', '\r', '\t'], " "))
                    .err()
                    .unwrap_or_default();
                self.pick = 0;
            }
            Stage::Chat => {
                self.notice = self.composer.insert(value).err().unwrap_or_default();
                self.command_pick = 0;
                self.history_pick = None;
            }
            Stage::Sessions(_) => {}
            Stage::Feedback(_) => {}
            Stage::Form(editor) => {
                if let Some(input) = editor.inputs.get_mut(editor.selected) {
                    let multiline = editor.form.fields[editor.indices[editor.selected]].multiline;
                    let value = if multiline {
                        value.to_string()
                    } else {
                        value.replace(['\n', '\r', '\t'], " ")
                    };
                    editor.error = input.insert(&value).err().unwrap_or_default();
                }
            }
        }
    }
    fn frame(&mut self, columns: u16, rows: u16) -> Frame {
        self.clicks.clear();
        let w = usize::from(columns);
        let h = usize::from(rows);
        let mut f = Frame::blank(w, h);
        if w < 64 || h < 20 {
            if let Some(view) = self.running.as_mut().and_then(|r| r.approval_view.as_mut()) {
                view.visible = false;
            }
            f.put(2, 2, "STACKPULSE", Tone::Accent);
            f.put(
                2,
                4,
                "Amplie para 64 × 20. Estado preservado.",
                Tone::Warning,
            );
            f.put(
                2,
                6,
                if self.selecting_text {
                    "COPIAR · F8/Esc voltar"
                } else {
                    "Ctrl+C sai · Esc cancela uma execução ativa"
                },
                Tone::Muted,
            );
            return f;
        }
        if self.draw_approval(&mut f) {
            return f;
        }
        let right = w - 3;
        let metrics_visible = matches!(self.stage, Stage::Chat) || self.settings_open;
        let has_detail = !self.settings_open
            && matches!(self.stage, Stage::Chat)
            && self.agent_view.selected.is_some();
        let full_detail = has_detail && (w < 110 || h < 28);
        let sidebar = (matches!(self.stage, Stage::Chat) && !self.settings_open)
            .then(|| {
                if has_detail && !full_detail {
                    Some((w * 45 / 100).max(48))
                } else {
                    chat_agents::width(w)
                }
            })
            .flatten();
        let chat_width = sidebar.map_or(right, |width| w - width - 6);
        let expanded = (matches!(self.stage, Stage::Chat)
            && self.agents_expanded
            && !self.settings_open
            && !has_detail)
            || full_detail;
        let widget_height = if h < 28 { 3 } else { 5 };
        let tab_y = if metrics_visible {
            widget_height + 1
        } else {
            0
        };
        let context_y = if metrics_visible { tab_y + 3 } else { 2 };
        let transcript_y = context_y + 3;
        let stats = metrics_visible.then(|| self.conversation_stats());
        if self.metrics_open
            && let Some(stats) = &stats
        {
            self.note = stats.detail_lines();
        }
        if let Some(stats) = &stats {
            let selected = self
                .metric_pick
                .unwrap_or_else(|| stats.prompts.len().saturating_sub(1));
            self.clicks.extend(
                chat_widgets::draw(
                    &mut f,
                    Rect {
                        x: 2,
                        y: 0,
                        width: w - 4,
                        height: widget_height,
                    },
                    stats,
                    selected,
                )
                .into_iter()
                .map(|(rect, action)| (rect, ClickAction::Widget(action))),
            );
        }
        if !expanded {
            put(
                &mut f,
                2,
                context_y,
                &format!("✦ STACKPULSE  ·  {}", tui::display_path(&self.cwd)),
                Tone::Navigation,
                chat_width,
            );
        }
        let order = self.tab_order();
        let active_position = order
            .iter()
            .position(|index| *index == self.active_tab)
            .unwrap_or(0);
        let labels = order
            .iter()
            .map(|&index| {
                let tab = &self.tabs[index];
                if index == self.active_tab {
                    TabLabel {
                        title: self.session.title.clone(),
                        running: self.running.is_some(),
                        unread: false,
                    }
                } else {
                    let tab = tab.as_ref().expect("inactive tab owns its state");
                    TabLabel {
                        title: tab.session.title.clone(),
                        running: tab.running.is_some(),
                        unread: tab.unread,
                    }
                }
            })
            .collect::<Vec<_>>();
        self.clicks.extend(
            chat_tabs::draw_with_settings(
                &mut f,
                tab_y,
                &labels,
                active_position,
                self.settings_open,
            )
            .into_iter()
            .map(|(rect, action)| {
                (
                    rect,
                    ClickAction::Tab(match action {
                        TabAction::Select(position) => TabAction::Select(order[position]),
                        other => other,
                    }),
                )
            }),
        );
        if self.settings_open {
            self.draw_settings(&mut f, context_y + 2);
            return f;
        }
        match &mut self.stage {
            Stage::Profiles => {
                put(&mut f, 2, 5, "Escolha sua equipe", Tone::Navigation, right);
                put(
                    &mut f,
                    2,
                    6,
                    "Escolha a equipe antes de enviar seu pedido.",
                    Tone::Muted,
                    right,
                );
                let (search, caret) = single(&self.search, right - 4);
                target(
                    &mut self.clicks,
                    2,
                    8,
                    right,
                    1,
                    ClickAction::Search(self.search.lines(right - 4).1.1),
                );
                put(
                    &mut f,
                    2,
                    8,
                    &format!(
                        "/ {}",
                        if search.is_empty() {
                            "Digite para filtrar…"
                        } else {
                            &search
                        }
                    ),
                    Tone::Muted,
                    right,
                );
                if !self.search.value.is_empty() {
                    f.caret = Some((4 + caret, 8));
                }
                let choices = self.choices();
                self.pick = self.pick.min(choices.len().saturating_sub(1));
                let capacity = ((h - 18) / 2).max(1);
                let first = self.pick.saturating_sub(capacity - 1);
                if choices.is_empty() {
                    put(
                        &mut f,
                        2,
                        10,
                        if self.catalog.notice.is_empty() {
                            "Nenhum perfil corresponde à busca."
                        } else {
                            &self.catalog.notice
                        },
                        Tone::Warning,
                        right,
                    );
                }
                for (offset, index) in choices.iter().skip(first).take(capacity).enumerate() {
                    let c = &self.catalog.choices[*index];
                    let selected = offset + first == self.pick;
                    let y = 10 + offset * 2;
                    target(
                        &mut self.clicks,
                        2,
                        y,
                        right,
                        2,
                        ClickAction::Profile(first + offset),
                    );
                    put(
                        &mut f,
                        2,
                        y,
                        &format!(
                            "{} {}{}   · {}",
                            if selected { "❯" } else { " " },
                            c.name,
                            if self.catalog.winner.as_deref() == Some(c.name.as_str()) {
                                " 🏆"
                            } else {
                                ""
                            },
                            c.scope
                        ),
                        if selected { Tone::Selected } else { Tone::Text },
                        right,
                    );
                    put(
                        &mut f,
                        4,
                        y + 1,
                        &c.subtitle,
                        if c.problem.is_some() {
                            Tone::Warning
                        } else {
                            Tone::Muted
                        },
                        right - 2,
                    );
                }
                if let Some(index) = choices.get(self.pick) {
                    let choice = &self.catalog.choices[*index];
                    for (y, line) in (h - 7..h - 4).zip(choice.detail.iter().take(3)) {
                        put(&mut f, 2, y, line, Tone::Muted, right);
                    }
                }
                if !self.notice.is_empty() {
                    put(&mut f, 2, h - 3, &self.notice, Tone::Warning, right);
                }
                put(
                    &mut f,
                    2,
                    h - 2,
                    if w < 86 {
                        "↑↓ perfil · Enter usar · F2 setup · F3 perfis · Esc sair"
                    } else {
                        "↑↓ escolher · Enter continuar · F2 setup · F3 perfis · Esc voltar/sair"
                    },
                    Tone::Accent,
                    right,
                );
            }
            Stage::Sessions(picker) => {
                put(
                    &mut f,
                    2,
                    5,
                    "Conversas salvas",
                    Tone::Accent,
                    right.saturating_sub(25),
                );
                let actions_x = right.saturating_sub(22);
                put(&mut f, actions_x, 5, "[Importar]", Tone::Primary, 10);
                target(
                    &mut self.clicks,
                    actions_x,
                    5,
                    10,
                    1,
                    ClickAction::ImportSession,
                );
                put(
                    &mut f,
                    actions_x + 12,
                    5,
                    "[Exportar]",
                    Tone::Navigation,
                    10,
                );
                target(
                    &mut self.clicks,
                    actions_x + 12,
                    5,
                    10,
                    1,
                    ClickAction::ExportSession,
                );
                put(
                    &mut f,
                    2,
                    6,
                    "Enter reabre · o histórico desta sessão acompanha os próximos pedidos ao agente.",
                    Tone::Muted,
                    right,
                );
                let capacity = ((h - 13) / 3).max(1);
                picker.selected = picker.selected.min(picker.sessions.len().saturating_sub(1));
                let first = picker.selected.saturating_sub(capacity - 1);
                if picker.sessions.is_empty() {
                    put(
                        &mut f,
                        2,
                        9,
                        "Ainda não há conversas salvas neste projeto.",
                        Tone::Muted,
                        right,
                    );
                }
                for (offset, session) in picker
                    .sessions
                    .iter()
                    .skip(first)
                    .take(capacity)
                    .enumerate()
                {
                    let index = first + offset;
                    let selected = index == picker.selected;
                    let y = 8 + offset * 3;
                    target(
                        &mut self.clicks,
                        2,
                        y,
                        right,
                        2,
                        ClickAction::Session(session.id.clone()),
                    );
                    put(
                        &mut f,
                        2,
                        y,
                        &format!(
                            "{} {}{}",
                            if selected { "❯" } else { " " },
                            session.title,
                            if session.id == self.session.id {
                                "  · atual"
                            } else {
                                ""
                            }
                        ),
                        if selected { Tone::Selected } else { Tone::Text },
                        right,
                    );
                    put(
                        &mut f,
                        4,
                        y + 1,
                        &format!(
                            "{} {} · atualizada {} · criada {}{}",
                            session.turns,
                            if session.turns == 1 {
                                "pedido"
                            } else {
                                "pedidos"
                            },
                            session.updated_at.format("%Y-%m-%d %H:%M UTC"),
                            session.created_at.format("%Y-%m-%d %H:%M UTC"),
                            if session.ended_at.is_some() {
                                " · encerrada"
                            } else {
                                ""
                            }
                        ),
                        Tone::Muted,
                        right - 2,
                    );
                }
                if !self.notice.is_empty() {
                    put(&mut f, 2, h - 3, &self.notice, Tone::Warning, right);
                }
                put(
                    &mut f,
                    2,
                    h - 2,
                    "↑↓ escolher · Enter abrir · I importar · E exportar · Esc voltar",
                    Tone::Accent,
                    right,
                );
                put(
                    &mut f,
                    right.saturating_sub(9),
                    h - 3,
                    "[ Voltar ]",
                    Tone::Accent,
                    10,
                );
                target(
                    &mut self.clicks,
                    right.saturating_sub(9),
                    h - 3,
                    10,
                    1,
                    ClickAction::Back,
                );
            }
            Stage::Feedback(picker) => {
                put(
                    &mut f,
                    2,
                    5,
                    "Respostas desta conversa",
                    Tone::Accent,
                    right,
                );
                put(
                    &mut f,
                    2,
                    6,
                    "Escolha um pedido; o conteúdo completo da resposta aparece abaixo.",
                    Tone::Muted,
                    right,
                );
                picker.selected = picker.selected.min(self.turns.len().saturating_sub(1));
                let capacity_limit = if h < 28 {
                    1
                } else {
                    ((h.saturating_sub(18)) / 2).clamp(2, 5)
                };
                let capacity = self.turns.len().clamp(1, capacity_limit);
                let first = picker.selected.saturating_sub(capacity - 1);
                if self.turns.is_empty() {
                    put(
                        &mut f,
                        2,
                        8,
                        "Ainda não há respostas nesta conversa.",
                        Tone::Muted,
                        right,
                    );
                }
                for (offset, turn) in self.turns.iter().skip(first).take(capacity).enumerate() {
                    let index = first + offset;
                    let selected = index == picker.selected;
                    let y = 8 + offset * 2;
                    let (state, state_tone, _) = feedback_state(turn);
                    target(
                        &mut self.clicks,
                        2,
                        y,
                        right,
                        2,
                        ClickAction::Feedback(index),
                    );
                    put(
                        &mut f,
                        2,
                        y,
                        &format!(
                            "{} #{} · {} · {state}",
                            if selected { "❯" } else { " " },
                            index + 1,
                            turn.status
                        ),
                        if selected { Tone::Selected } else { state_tone },
                        right,
                    );
                    put(
                        &mut f,
                        4,
                        y + 1,
                        &format!("Pedido · {}", turn.prompt),
                        Tone::Muted,
                        right.saturating_sub(2),
                    );
                }

                let detail_top = 9 + capacity * 2;
                let detail_bottom = h.saturating_sub(5);
                let detail_height = detail_bottom.saturating_sub(detail_top);
                if let Some(turn) = self.turns.get(picker.selected) {
                    let lines = feedback_detail_lines(turn, right.saturating_sub(2));
                    let max = lines.len().saturating_sub(detail_height);
                    picker.scroll = picker.scroll.min(max);
                    target(
                        &mut self.clicks,
                        2,
                        detail_top,
                        right,
                        detail_height,
                        ClickAction::FeedbackContent,
                    );
                    for (y, (line, tone)) in
                        (detail_top..detail_bottom).zip(lines.iter().skip(picker.scroll))
                    {
                        put(&mut f, 2, y, line, *tone, right);
                    }
                    if max > 0 {
                        put(
                            &mut f,
                            right.saturating_sub(16),
                            detail_top - 1,
                            &format!(
                                "linhas {}–{} / {}",
                                picker.scroll + 1,
                                (picker.scroll + detail_height).min(lines.len()),
                                lines.len()
                            ),
                            Tone::Muted,
                            17,
                        );
                    }
                    let (_, _, can_rate) = feedback_state(turn);
                    let label = if can_rate {
                        "[ Avaliar selecionada ]"
                    } else {
                        "[ Avaliação indisponível ]"
                    };
                    put(
                        &mut f,
                        2,
                        h - 3,
                        label,
                        if can_rate { Tone::Primary } else { Tone::Muted },
                        27,
                    );
                    if can_rate {
                        target(
                            &mut self.clicks,
                            2,
                            h - 3,
                            label.chars().count(),
                            1,
                            ClickAction::FeedbackSubmit,
                        );
                    }
                }
                if !self.notice.is_empty() {
                    put(&mut f, 2, h - 4, &self.notice, Tone::Warning, right);
                }
                put(
                    &mut f,
                    right.saturating_sub(9),
                    h - 3,
                    "[ Voltar ]",
                    Tone::Accent,
                    10,
                );
                target(
                    &mut self.clicks,
                    right.saturating_sub(9),
                    h - 3,
                    10,
                    1,
                    ClickAction::Back,
                );
                put(
                    &mut f,
                    2,
                    h - 2,
                    "↑↓ escolher · Enter avaliar · PgUp/PgDn ou roda ler resposta · Esc voltar",
                    Tone::Muted,
                    right,
                );
            }
            Stage::Form(editor) => {
                put(&mut f, 2, 5, &editor.form.title, Tone::Accent, right);
                let capacity = ((h - 13) / 3).max(1);
                let first = editor.selected.saturating_sub(capacity - 1);
                for offset in first..editor.indices.len().min(first + capacity) {
                    let field = &editor.form.fields[editor.indices[offset]];
                    let y = 7 + (offset - first) * 3;
                    put(&mut f, 2, y, &field.label, Tone::Muted, right);
                    let (value, caret) = single(&editor.inputs[offset], right - 4);
                    target(
                        &mut self.clicks,
                        2,
                        y,
                        right,
                        2,
                        ClickAction::FormField(offset, editor.inputs[offset].lines(right - 4).1.1),
                    );
                    put(
                        &mut f,
                        2,
                        y + 1,
                        &format!(
                            "{} {}",
                            if offset == editor.selected {
                                "❯"
                            } else {
                                " "
                            },
                            if value.is_empty() { "…" } else { &value }
                        ),
                        if offset == editor.selected {
                            Tone::Selected
                        } else {
                            Tone::Text
                        },
                        right,
                    );
                    if offset == editor.selected {
                        f.caret = Some((4 + caret, y + 1));
                    }
                }
                put(
                    &mut f,
                    2,
                    h - 5,
                    match editor.transfer {
                        Some(Transfer::Import) => "[ Importar ]",
                        Some(Transfer::Export(_)) => "[ Exportar ]",
                        None => "[ Salvar ]",
                    },
                    if editor.selected == editor.inputs.len() {
                        Tone::Selected
                    } else {
                        Tone::Primary
                    },
                    right,
                );
                target(&mut self.clicks, 2, h - 5, 12, 1, ClickAction::Save);
                put(&mut f, 15, h - 5, "[ Voltar ]", Tone::Accent, 10);
                target(&mut self.clicks, 15, h - 5, 10, 1, ClickAction::Back);
                let hint = editor
                    .indices
                    .get(editor.selected)
                    .map(|i| editor.form.fields[*i].hint.as_str())
                    .unwrap_or("Enter salva as alterações.");
                put(
                    &mut f,
                    2,
                    h - 3,
                    if editor.error.is_empty() {
                        hint
                    } else {
                        &editor.error
                    },
                    if editor.error.is_empty() {
                        Tone::Muted
                    } else {
                        Tone::Warning
                    },
                    right,
                );
                put(
                    &mut f,
                    2,
                    h - 2,
                    "Tab / Enter próximo · Shift+Tab voltar · Ctrl+U limpar · F5 salvar · Esc voltar",
                    Tone::Muted,
                    right,
                );
            }
            Stage::Chat => {
                if full_detail || (expanded && !has_detail) {
                    let area = Rect {
                        x: 2,
                        y: context_y,
                        width: w - 4,
                        height: h.saturating_sub(context_y + 4),
                    };
                    if full_detail {
                        self.agent_view.focused = true;
                        self.draw_agent_detail(&mut f, area);
                    } else {
                        put(
                            &mut f,
                            area.x,
                            area.y,
                            "[Sessões]",
                            Tone::Navigation,
                            area.width,
                        );
                        target(
                            &mut self.clicks,
                            area.x,
                            area.y,
                            9,
                            1,
                            ClickAction::Sessions,
                        );
                        let agents_y = area.y + 2;
                        let agents_height = area.height.saturating_sub(2);
                        target(
                            &mut self.clicks,
                            area.x,
                            agents_y,
                            area.width,
                            agents_height,
                            ClickAction::Agents,
                        );
                        self.clicks.extend(
                            chat_agents::draw(
                                &mut f,
                                area.x,
                                agents_y,
                                area.width,
                                agents_height,
                                &self.activity,
                                &mut self.agent_scroll,
                            )
                            .into_iter()
                            .map(|(rect, id)| (rect, ClickAction::Agent(id))),
                        );
                    }
                    put(&mut f, 2, h - 3, &self.notice, Tone::Warning, right);
                    put(
                        &mut f,
                        2,
                        h - 2,
                        if full_detail {
                            "Enter orientar · F9 redirecionar · Esc voltar"
                        } else {
                            "F4 / Esc voltar · Alt+↑↓ agentes · Clique para abrir"
                        },
                        Tone::Muted,
                        right,
                    );
                    return f;
                }
                if let Some(chosen) = &self.chosen
                    && !expanded
                {
                    let choice_width = if sidebar.is_none() && !has_detail {
                        chat_width.saturating_sub(11)
                    } else {
                        chat_width
                    };
                    put(
                        &mut f,
                        2,
                        context_y + 1,
                        &format!(
                            "{} · {} · {} / {}",
                            chosen.name, chosen.client, chosen.model, chosen.effort
                        ),
                        Tone::Muted,
                        choice_width,
                    );
                }
                if sidebar.is_none() && !expanded && !has_detail {
                    let sessions_label = "[Sessões]";
                    let sessions_width = sessions_label.chars().count();
                    let sessions_x = w.saturating_sub(sessions_width + 2);
                    put(
                        &mut f,
                        sessions_x,
                        context_y + 1,
                        sessions_label,
                        Tone::Navigation,
                        sessions_width,
                    );
                    target(
                        &mut self.clicks,
                        sessions_x,
                        context_y + 1,
                        sessions_width,
                        1,
                        ClickAction::Sessions,
                    );
                    let label = format!("[Subagentes {} · F4]", self.activity.agents.len());
                    let width = label.chars().count();
                    let x = w.saturating_sub(width + 2);
                    put(&mut f, x, context_y + 2, &label, Tone::Navigation, width);
                    target(
                        &mut self.clicks,
                        x,
                        context_y + 2,
                        width,
                        1,
                        ClickAction::Agents,
                    );
                }
                let (input, (cx, cy)) = self.composer.lines(right - 4);
                let input_height = input.len().clamp(1, 5);
                let input_top = h - 5 - input_height;
                let palette = self.suggestions();
                let palette_height = palette
                    .len()
                    .min(4)
                    .min(input_top.saturating_sub(transcript_y + 1));
                let transcript_bottom = input_top.saturating_sub(palette_height + 1);
                let mut lines: Vec<TranscriptLine> = vec![];
                if self.turns.len() == self.hidden_turns {
                    lines.push(TranscriptLine::Text(
                        "Pronto para começar".into(),
                        Tone::Navigation,
                    ));
                    lines.push(TranscriptLine::Text(
                        "Descreva o que você precisa. Use /help para ver os comandos.".into(),
                        Tone::Muted,
                    ));
                    if self.chosen.as_ref().is_some_and(|c| !c.compiled) {
                        lines.push(TranscriptLine::Text(
                            "A imagem selecionada será interpretada ao enviar o primeiro pedido."
                                .into(),
                            Tone::Warning,
                        ));
                    }
                }
                for turn in self.turns.iter().skip(self.hidden_turns) {
                    lines.push(TranscriptLine::Text(
                        format!("❯ Você · {}", turn.profile),
                        Tone::Accent,
                    ));
                    lines.extend(
                        wrap(&turn.prompt, chat_width - 2)
                            .into_iter()
                            .map(|l| TranscriptLine::Text(format!("  {l}"), Tone::Text)),
                    );
                    lines.push(TranscriptLine::Text(String::new(), Tone::Text));
                    lines.push(TranscriptLine::Text(
                        format!("✦ {}", turn.status),
                        if turn.status.starts_with("Cancelado")
                            || turn.status.starts_with("Falhou")
                            || turn.status.starts_with("Erro")
                        {
                            Tone::Danger
                        } else if turn.finished {
                            Tone::Success
                        } else {
                            Tone::Warning
                        },
                    ));
                    for line in &turn.lines {
                        lines.extend(
                            wrap(line, chat_width)
                                .into_iter()
                                .map(|l| TranscriptLine::Text(l, Tone::Text)),
                        );
                    }
                    if let Some(job) = &turn.execution {
                        lines.extend(
                            wrap(&metrics(job), chat_width)
                                .into_iter()
                                .map(|l| TranscriptLine::Text(l, Tone::Muted)),
                        );
                    }
                    lines.push(TranscriptLine::Text(String::new(), Tone::Text));
                }
                let note_start = lines.len();
                for line in &self.note {
                    lines.extend(
                        wrap(line, chat_width)
                            .into_iter()
                            .map(|l| TranscriptLine::Text(l, Tone::Muted)),
                    );
                }
                let capacity = transcript_bottom.saturating_sub(transcript_y);
                let max = lines.len().saturating_sub(capacity);
                if self.focus_note {
                    self.scroll = note_start;
                    self.focus_note = false;
                }
                if self.follow {
                    self.scroll = max;
                } else {
                    self.scroll = self.scroll.min(max);
                }
                if !expanded {
                    target(
                        &mut self.clicks,
                        2,
                        transcript_y,
                        chat_width,
                        transcript_bottom.saturating_sub(transcript_y),
                        ClickAction::Transcript,
                    );
                    for (y, line) in
                        (transcript_y..transcript_bottom).zip(lines.iter().skip(self.scroll))
                    {
                        let TranscriptLine::Text(text, tone) = line;
                        put(&mut f, 2, y, text, *tone, chat_width);
                    }
                }
                if !has_detail && (expanded || sidebar.is_some()) {
                    let width = if expanded { w - 4 } else { sidebar.unwrap() };
                    let x = if expanded { 2 } else { w - width - 2 };
                    put(&mut f, x, context_y, "[Sessões]", Tone::Navigation, width);
                    target(&mut self.clicks, x, context_y, 9, 1, ClickAction::Sessions);
                    let agents_y = context_y + 2;
                    let height = transcript_bottom.saturating_sub(agents_y);
                    target(
                        &mut self.clicks,
                        x,
                        agents_y,
                        width,
                        height,
                        ClickAction::Agents,
                    );
                    self.clicks.extend(
                        chat_agents::draw(
                            &mut f,
                            x,
                            agents_y,
                            width,
                            height,
                            &self.activity,
                            &mut self.agent_scroll,
                        )
                        .into_iter()
                        .map(|(rect, id)| (rect, ClickAction::Agent(id))),
                    );
                }
                let first = self
                    .command_pick
                    .saturating_sub(palette_height.saturating_sub(1));
                for (offset, (cmd, description)) in
                    palette.iter().skip(first).take(palette_height).enumerate()
                {
                    target(
                        &mut self.clicks,
                        2,
                        input_top - palette_height + offset,
                        right,
                        1,
                        ClickAction::Suggestion(cmd),
                    );
                    put(
                        &mut f,
                        2,
                        input_top - palette_height + offset,
                        &format!("{cmd:<12} {description}"),
                        if first + offset == self.command_pick {
                            Tone::Selected
                        } else {
                            Tone::Muted
                        },
                        right,
                    );
                }
                put(&mut f, 2, input_top, &"─".repeat(right), Tone::Muted, right);
                put(
                    &mut f,
                    3,
                    input_top,
                    if self.running.is_some() {
                        " EM EXECUÇÃO "
                    } else {
                        " SEU PEDIDO "
                    },
                    if self.running.is_some() {
                        Tone::Warning
                    } else {
                        Tone::Navigation
                    },
                    right - 2,
                );
                if max > 0 && capacity > 0 && palette.is_empty() {
                    let progress = if self.follow || self.scroll == max {
                        " Últimas mensagens ".to_string()
                    } else {
                        format!(
                            " {}–{} / {} · PgDn: avançar ",
                            self.scroll + 1,
                            (self.scroll + capacity).min(lines.len()),
                            lines.len()
                        )
                    };
                    let progress_width = unicode_width::UnicodeWidthStr::width(progress.as_str());
                    if progress_width + 18 < right {
                        put(
                            &mut f,
                            w - progress_width - 3,
                            input_top,
                            &progress,
                            Tone::Muted,
                            progress_width,
                        );
                    }
                }
                let input_first = cy.saturating_sub(input_height - 1);
                target(
                    &mut self.clicks,
                    4,
                    input_top + 1,
                    right - 4,
                    input_height,
                    ClickAction::Composer {
                        first: input_first,
                        width: right - 4,
                    },
                );
                if self.running.is_some() {
                    put(
                        &mut f,
                        2,
                        input_top + 1,
                        "● Equipe trabalhando…  Esc para cancelar",
                        Tone::Muted,
                        right,
                    );
                } else {
                    for (offset, line) in input
                        .iter()
                        .skip(input_first)
                        .take(input_height)
                        .enumerate()
                    {
                        let placeholder = self.composer.value.is_empty();
                        let text = if placeholder {
                            "Envie um pedido ou /comando"
                        } else {
                            line.as_str()
                        };
                        put(
                            &mut f,
                            2,
                            input_top + 1 + offset,
                            &format!("{} {text}", if offset == 0 { "❯" } else { " " }),
                            if placeholder { Tone::Muted } else { Tone::Text },
                            right,
                        );
                    }
                    f.caret = Some((4 + cx, input_top + 1 + cy - input_first));
                }
                put(&mut f, 2, h - 4, &"─".repeat(right), Tone::Muted, right);
                put(
                    &mut f,
                    3,
                    h - 4,
                    " Alt+Enter/Ctrl+J: linha · F8 copiar ",
                    Tone::Muted,
                    right - 17,
                );
                let label = if self.running.is_some() {
                    "[ Cancelar ]"
                } else {
                    "[ Enviar ]"
                };
                let width = label.chars().count();
                let x = w - width - 2;
                put(
                    &mut f,
                    x,
                    h - 4,
                    label,
                    if self.running.is_some() {
                        Tone::Danger
                    } else {
                        Tone::Primary
                    },
                    width,
                );
                target(
                    &mut self.clicks,
                    x,
                    h - 4,
                    width,
                    1,
                    if self.running.is_some() {
                        ClickAction::Cancel
                    } else {
                        ClickAction::Submit
                    },
                );
                put(
                    &mut f,
                    2,
                    h - 3,
                    &self.notice,
                    if self.notice.starts_with('/') {
                        Tone::Muted
                    } else {
                        Tone::Warning
                    },
                    right,
                );
                if let Some(width) = sidebar {
                    let x = w - width - 4;
                    for y in context_y..transcript_bottom {
                        put(&mut f, x, y, "│", Tone::Muted, 1);
                    }
                }
                if has_detail {
                    let width = if full_detail { w - 4 } else { sidebar.unwrap() };
                    let x = if full_detail { 2 } else { w - width - 2 };
                    self.draw_agent_detail(
                        &mut f,
                        Rect {
                            x,
                            y: context_y,
                            width,
                            height: transcript_bottom.saturating_sub(context_y),
                        },
                    );
                }
                let requests = self.turns.len();
                let shortcuts = if has_detail {
                    "Enter orientar · Ctrl+J linha · F8 copiar · F9 redirecionar · Esc voltar"
                        .into()
                } else if expanded {
                    "F4 / Esc voltar · Alt+↑↓ agentes · Ctrl+C cancelar/sair".into()
                } else if w < 86 {
                    "Enter enviar · Ctrl+J linha · F8 copiar · F6 abas".into()
                } else {
                    format!(
                        "{requests} {} · Enter enviar · Ctrl+J linha · F8 copiar · F6 abas · /help",
                        if requests == 1 { "pedido" } else { "pedidos" }
                    )
                };
                put(&mut f, 2, h - 2, &shortcuts, Tone::Muted, right);
            }
        }
        if self.selecting_text {
            f.caret = None;
            put(&mut f, 2, h - 2, &" ".repeat(w - 4), Tone::Muted, w - 4);
            put(
                &mut f,
                2,
                h - 2,
                "COPIAR: arraste e use Copiar do terminal · F8/Esc voltar",
                Tone::Warning,
                w - 4,
            );
        }
        f
    }
}

fn is_newline(key: KeyEvent) -> bool {
    (key.code == KeyCode::Enter
        && key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT))
        || (matches!(key.code, KeyCode::Char('j' | 'J'))
            && key.modifiers.contains(KeyModifiers::CONTROL))
}

fn execution(db: &Db, id: &str) -> Result<Option<Execution>> {
    let data: Option<String> =
        db.0.query_row("SELECT data FROM executions WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    data.map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}
fn answer_lines(lines: &[String], id: Option<&str>) -> Vec<String> {
    lines
        .iter()
        .filter(|line| {
            !line.starts_with("stderr: AI Timeline")
                && !line.starts_with("stderr: Telemetria")
                && !id.is_some_and(|id| line.starts_with(&format!("Registro {id} ·")))
        })
        .map(|line| {
            line.strip_prefix("stderr: ")
                .map_or_else(|| line.clone(), |s| format!("CLI: {s}"))
        })
        .collect()
}
fn feedback_state(turn: &Turn) -> (String, Tone, bool) {
    match turn.execution.as_ref() {
        Some(job) if job.ended_at.is_some() => {
            if let Some(feedback) = &job.feedback {
                (
                    format!(
                        "Avaliada · entrega {} · rapidez {}",
                        format!("{:.1}", feedback.delivered).replace('.', ","),
                        if (1..=5).contains(&feedback.speed) {
                            format!("{}/5", feedback.speed)
                        } else {
                            "não avaliada".into()
                        }
                    ),
                    Tone::Success,
                    true,
                )
            } else {
                ("Disponível para avaliar".into(), Tone::Navigation, true)
            }
        }
        Some(_) => (
            "Em andamento · avaliação indisponível".into(),
            Tone::Warning,
            false,
        ),
        None if !turn.finished => (
            "Em andamento · sem registro de execução".into(),
            Tone::Warning,
            false,
        ),
        None => (
            "Sem registro de execução · avaliação indisponível".into(),
            Tone::Muted,
            false,
        ),
    }
}
fn feedback_detail_lines(turn: &Turn, width: usize) -> Vec<(String, Tone)> {
    let mut lines = vec![("CONTEXTO DO PEDIDO".into(), Tone::Accent)];
    lines.extend(
        wrap(&turn.prompt, width)
            .into_iter()
            .map(|line| (line, Tone::Text)),
    );
    lines.push((String::new(), Tone::Text));
    lines.push((format!("RESPOSTA · {}", turn.status), Tone::Accent));
    if turn.lines.is_empty() {
        lines.push(("Sem conteúdo de resposta registrado.".into(), Tone::Muted));
    } else {
        for response in &turn.lines {
            lines.extend(
                wrap(response, width)
                    .into_iter()
                    .map(|line| (line, Tone::Text)),
            );
        }
    }
    lines.push((String::new(), Tone::Text));
    let (state, tone, _) = feedback_state(turn);
    lines.push((format!("AVALIAÇÃO · {state}"), tone));
    if let Some(job) = &turn.execution {
        lines.push((format!("Execução · {}", job.id), Tone::Muted));
    }
    lines
}
fn metrics(job: &Execution) -> String {
    let tokens = job
        .metrics
        .as_ref()
        .filter(|m| m.events > 0)
        .map(|m| m.total_tokens)
        .or(job.reported_tokens.map(|t| t.total()));
    let cost = job
        .metrics
        .as_ref()
        .and_then(|m| m.estimated_usd)
        .or(job.reported_cost_usd);
    format!(
        "{} tokens · {} · {} · {}",
        tokens.map(widget::number).unwrap_or_else(|| "?".into()),
        job.wall_ms
            .map(|ms| widget::duration(ms as i64))
            .unwrap_or_else(|| "em andamento".into()),
        cost.map(|n| format!("~US$ {n:.4}"))
            .unwrap_or_else(|| "custo indisponível".into()),
        job.execution_client.label()
    )
}
fn put(f: &mut Frame, x: usize, y: usize, text: &str, tone: Tone, width: usize) {
    f.put(x, y, &widget::clean(text, width), tone);
}
fn target(
    clicks: &mut Vec<(Rect, ClickAction)>,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    action: ClickAction,
) {
    if width > 0 && height > 0 {
        clicks.push((
            Rect {
                x,
                y,
                width,
                height,
            },
            action,
        ));
    }
}
fn wrap(text: &str, width: usize) -> Vec<String> {
    Composer {
        value: text.into(),
        cursor: 0,
    }
    .lines(width)
    .0
}
fn single(input: &Composer, width: usize) -> (String, usize) {
    let (lines, (x, y)) = input.lines(width);
    (lines.get(y).cloned().unwrap_or_default(), x)
}
struct Signals(Vec<signal_hook::SigId>);
impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.0.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

pub fn run(db: &mut Db, options: Options) -> Result<()> {
    ensure!(
        tui::available(),
        "Use ui em um terminal interativo. Os comandos textuais continuam disponíveis para scripts."
    );
    let cwd = options.cwd.clone();
    let mut chat = Chat::new(options, cwd)?;
    chat.history.restore_execution_cache(db)?;
    let mut signals = Signals(vec![]);
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signals.0.push(signal_hook::flag::register(
            signal,
            chat.interrupted.clone(),
        )?);
    }
    let mut guard = Some(TerminalGuard::enter_with_mouse()?);
    let result = (|| -> Result<()> {
        let mut selection_frame_size = None;
        loop {
            if chat.interrupted.load(Ordering::Relaxed) {
                break;
            }
            if chat.poll_all(db)
                && let Some(guard) = &guard
            {
                guard.refresh_input()?;
            }
            let (w, h) = terminal::size()?;
            // Poll jobs normally, but avoid destroying native selections with
            // redraws. Resize and entering/leaving selection still redraw once.
            if !chat.selecting_text || selection_frame_size != Some((w, h)) {
                chat.frame(w, h).draw(w, h, tui::colors())?;
                selection_frame_size = chat.selecting_text.then_some((w, h));
            }
            if !event::poll(Duration::from_millis(if chat.any_running() {
                150
            } else {
                1000
            }))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) => {
                    if (w < 64 || h < 20)
                        && key.code != KeyCode::Esc
                        && key.code != KeyCode::F(8)
                        && !(key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL))
                    {
                        continue;
                    }
                    if chat.key(key, db, &mut guard)? {
                        break;
                    }
                }
                Event::Paste(value) if w >= 64 && h >= 20 => chat.paste(&value),
                Event::Mouse(event) if w >= 64 && h >= 20 && !chat.selecting_text => {
                    match chat.mouse(event, db, &mut guard) {
                        Ok(true) => break,
                        Ok(false) => {}
                        Err(error) => chat.notice = format!("{error:#}"),
                    }
                }
                _ => {}
            }
        }
        Ok(())
    })();
    let finish = chat.finish_all(db);
    drop(guard);
    result?;
    finish?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{client::Backend, profiles::Settings};
    use std::path::Path;

    fn fixture(dir: &Path) -> (Chat, Db) {
        let config = dir.join("config.json");
        let providers = dir.join("providers");
        let all = providers.join("project/all");
        std::fs::create_dir_all(&all).unwrap();
        std::fs::write(all.join("selected.png"), b"image fixture").unwrap();
        Settings {
            schema_version: 1,
            client: Backend::Codex,
            provider: "openai".into(),
            model: "gpt-6-astra".into(),
            effort: "medium".into(),
            executable: "must-not-be-launched".into(),
            providers_root: providers,
            default_profile: Some("selected".into()),
            max_agents: 4,
            ai_memory: false,
            ai_usagebar: false,
        }
        .save(&config)
        .unwrap();
        let options = Options {
            no_policy: false,
            cwd: dir.into(),
            db: dir.join("usage.sqlite"),
            config,
            sessions: dir.join("sessions"),
            timezone: chrono_tz::UTC,
            page: Page::Run,
        };
        let db = Db::open(&options.db).unwrap();
        let mut chat = Chat::new(options, dir.into()).unwrap();
        chat.browser_opener = |_| Ok(());
        (chat, db)
    }
    #[test]
    fn skip_dangerous_chat_commands_control_future_job_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        for command in [
            "/skip-dangerous",
            "/no-policy",
            "/dangerously-skip-permissions",
        ] {
            chat.composer.value = command.into();
            assert!(chat.suggestions().iter().any(|(name, _)| *name == command));
            chat.command(&mut db, &mut None).unwrap();
            assert!(
                chat.arguments(vec!["run".into()])
                    .contains(&"--no-policy".into())
            );
            chat.composer.value = format!("{command} off");
            chat.command(&mut db, &mut None).unwrap();
            assert!(
                !chat
                    .arguments(vec!["run".into()])
                    .contains(&"--no-policy".into())
            );
            chat.composer.value = format!("{command} invalid");
            assert!(chat.command(&mut db, &mut None).is_err());
            assert!(!chat.options.no_policy);
            chat.composer.value = format!("{command} on");
            chat.command(&mut db, &mut None).unwrap();
            assert!(chat.options.no_policy);
        }
    }

    #[test]
    fn no_policy_is_forwarded_to_chat_jobs_only_when_selected() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, _) = fixture(dir.path());
        assert!(
            !chat
                .arguments(vec!["run".into()])
                .contains(&"--no-policy".into())
        );
        chat.options.no_policy = true;
        assert!(
            chat.arguments(vec!["run".into()])
                .contains(&"--no-policy".into())
        );
    }

    #[test]
    #[cfg(unix)]
    fn mermaid_responses_open_automatically_once_after_completion() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, db) = fixture(dir.path());
        chat.choose();
        assert!(!COMMANDS.iter().any(|(name, _)| *name == "/render"));
        for (script, expected, preview) in [
            ("printf 'plain answer\\n'", false, false),
            ("printf '```mermaid\\ngraph TD; A-->B\\n'", false, false),
            (
                "printf '```mermaid\\ngraph TD; A-->B\\n```\\n'",
                true,
                false,
            ),
            (
                "printf '```mermaid\\ngraph TD; A-->B\\n```\\n'",
                false,
                true,
            ),
        ] {
            chat.note.clear();
            fake_tab_job(&mut chat, "Resposta", script);
            chat.running.as_mut().unwrap().preview = preview;
            let deadline = Instant::now() + Duration::from_secs(5);
            while chat.running.is_some() {
                chat.poll(&db);
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                chat.note
                    .iter()
                    .any(|line| line.contains("Abertura automática")),
                expected
            );
            let note = chat.note.clone();
            chat.poll(&db);
            chat.frame(100, 32);
            assert_eq!(
                chat.note, note,
                "polling and redraw must not open another preview"
            );
            if expected {
                let path = note[0].split_once(": ").unwrap().1;
                assert!(std::path::Path::new(path).exists());
                std::fs::remove_file(path).unwrap();
            }
        }
    }

    #[test]
    fn profile_picker_shows_trophy_without_changing_profile_identifier() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, _) = fixture(dir.path());
        chat.catalog.winner = Some("selected".into());
        let frame = chat.frame(100, 32);
        assert!(
            frame
                .spans
                .iter()
                .any(|span| span.text.contains("selected 🏆"))
        );
        assert_eq!(chat.catalog.choices[0].name, "selected");
        chat.catalog.winner = None;
        assert!(
            !chat
                .frame(100, 32)
                .spans
                .iter()
                .any(|span| span.text.contains('🏆'))
        );
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[cfg(unix)]
    fn approval_fixture(chat: &mut Chat) -> (PathBuf, String) {
        fake_tab_job(chat, "Permissão", "exec sleep 30");
        let path_file = chat.cwd.join("mailbox-path");
        chat.running.as_mut().unwrap().job = Job::start_tracked(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "printf %s \"$STACKPULSE_CONTROL_DIR\" > \"$1\"; exec sleep 30".into(),
                "fixture".into(),
                path_file.as_os_str().to_owned(),
            ],
            &chat.cwd,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let path = loop {
            if let Ok(value) = std::fs::read_to_string(&path_file)
                && !value.is_empty()
            {
                break PathBuf::from(value).join("approvals");
            }
            assert!(Instant::now() < deadline, "fixture did not publish mailbox");
            std::thread::sleep(Duration::from_millis(5));
        };
        let id = uuid::Uuid::new_v4().to_string();
        std::fs::write(
            path.join(format!("{id}.json")),
            serde_json::to_vec(&crate::agent_control::ApprovalRequest {
                request_id: id.clone(),
                method: "item/commandExecution/requestApproval".into(),
                params: serde_json::json!({
                    "threadId": "thread-local", "command": "git commit -m teste",
                    "cwd": "/projeto", "reason": "Registrar perfis",
                    "changes": (0..40).map(|i| format!("change-{i}")).collect::<Vec<_>>(),
                    "z_last": "final-detail",
                }),
            })
            .unwrap(),
        )
        .unwrap();
        (path, id)
    }

    #[test]
    #[cfg(unix)]
    fn approvals_require_explicit_press_visible_details_and_preserve_draft() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.choose();
        let (path, id) = approval_fixture(&mut chat);
        let decision = path.join(format!("{id}.decision"));
        chat.composer.value = "rascunho preservado".into();
        let approve = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
        chat.key(approve, &mut db, &mut None).unwrap();
        assert!(!decision.exists(), "request must be rendered first");
        let frame = chat.frame(100, 30);
        let text = frame
            .spans
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains(&id) && text.contains("Ctrl+Y") && text.contains("Ctrl+N"));
        assert!(!text.contains("final-detail"));
        for event in [
            key(KeyCode::Enter),
            key(KeyCode::Char('y')),
            key(KeyCode::Char('n')),
            KeyEvent::new_with_kind(
                KeyCode::Char('y'),
                KeyModifiers::CONTROL,
                KeyEventKind::Repeat,
            ),
        ] {
            chat.key(event, &mut db, &mut None).unwrap();
            assert!(!decision.exists());
        }
        chat.paste("não deve substituir o rascunho");
        chat.key(key(KeyCode::End), &mut db, &mut None).unwrap();
        let frame = chat.frame(100, 30);
        let text = frame
            .spans
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("final-detail") && text.contains("thread-local"));
        assert!(text.contains("Registrar perfis") && text.contains("/projeto"));
        chat.frame(40, 12);
        chat.key(approve, &mut db, &mut None).unwrap();
        assert!(!decision.exists(), "small terminal must disable approval");
        chat.frame(100, 30);
        chat.key(approve, &mut db, &mut None).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&decision).unwrap())
                .unwrap()["accept"],
            true
        );
        assert_eq!(chat.composer.value, "rascunho preservado");
        assert!(
            chat.running
                .as_ref()
                .unwrap()
                .job
                .pending_approvals()
                .is_empty()
        );
    }

    #[test]
    #[cfg(unix)]
    fn approvals_are_scoped_to_active_tab_and_denial_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.choose();
        let (path, id) = approval_fixture(&mut chat);
        let decision = path.join(format!("{id}.decision"));
        let request_file = path.join(format!("{id}.json"));
        let mut request: crate::agent_control::ApprovalRequest =
            serde_json::from_slice(&std::fs::read(&request_file).unwrap()).unwrap();
        request.method = "item/permissions/requestApproval".into();
        request.params = serde_json::json!({"threadId":"thread-local", "permissions":{"network":{"enabled":true}}});
        std::fs::write(&request_file, serde_json::to_vec(&request).unwrap()).unwrap();
        assert!(
            chat.frame(100, 30)
                .spans
                .iter()
                .any(|s| s.text.contains("nesta rodada"))
        );
        chat.new_session().unwrap();
        assert!(
            !chat
                .frame(100, 30)
                .spans
                .iter()
                .any(|s| s.text.contains("PERMISSÃO NECESSÁRIA"))
        );
        chat.key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &mut db,
            &mut None,
        )
        .unwrap();
        assert!(!decision.exists());
        chat.switch_tab(0);
        let deny = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL);
        chat.key(deny, &mut db, &mut None).unwrap();
        assert!(
            !decision.exists(),
            "switched tab must render its request first"
        );
        chat.frame(100, 30);
        chat.key(deny, &mut db, &mut None).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(decision).unwrap())
                .unwrap()["accept"],
            false
        );
        assert_eq!(
            chat.tabs.len(),
            2,
            "Ctrl+N denies instead of creating a tab"
        );
        assert!(!chat.running.as_ref().unwrap().job.cancelling());
    }

    fn click(chat: &mut Chat, db: &mut Db, x: usize, y: usize) {
        chat.mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x as u16,
                row: y as u16,
                modifiers: KeyModifiers::NONE,
            },
            db,
            &mut None,
        )
        .unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn recent_tabs_keep_identity_and_titles_drafts_and_keyboard_follow_visual_order() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.choose();
        chat.paste("Rascunho preservado");
        let first_id = chat.session.id.clone();
        chat.frame(148, 38);
        let rename = chat
            .clicks
            .iter()
            .find(|(_, action)| matches!(action, ClickAction::Tab(TabAction::Rename)))
            .unwrap()
            .0;
        click(&mut chat, &mut db, rename.x, rename.y);
        chat.key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            &mut db,
            &mut None,
        )
        .unwrap();
        chat.paste("Documentação 界");
        chat.key(key(KeyCode::F(5)), &mut db, &mut None).unwrap();
        assert_eq!(chat.session.title, "Documentação 界");
        assert_eq!(chat.composer.value, "Rascunho preservado");
        chat.new_session().unwrap();
        chat.new_session().unwrap();
        assert_eq!(chat.tab_order(), vec![2, 1, 0]);
        chat.tab_action(TabAction::Next).unwrap();
        assert_eq!(chat.active_tab, 1);
        chat.key(
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT),
            &mut db,
            &mut None,
        )
        .unwrap();
        assert_eq!(chat.active_tab, 2);
        chat.tab_action(TabAction::Select(0)).unwrap();
        assert_eq!(chat.tab_order(), vec![2, 1, 0]);
        assert_eq!(chat.composer.value, "Rascunho preservado");
        let fake = dir.path().join("offline-job");
        std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        chat.executable = fake;
        chat.send("Revisar as páginas".into(), false).unwrap();
        assert_eq!(chat.tab_order(), vec![0, 2, 1]);
        assert_eq!(chat.session.id, first_id);
        assert_eq!(chat.session.title, "Documentação 界");
        chat.frame(148, 38);
        let ids: Vec<_> = chat
            .clicks
            .iter()
            .filter_map(|(_, action)| {
                if let ClickAction::Tab(TabAction::Select(index)) = action {
                    Some(*index)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(ids, vec![0, 2, 1]);
        // Renaming a running conversation changes only its metadata.
        chat.tab_action(TabAction::Rename).unwrap();
        chat.key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            &mut db,
            &mut None,
        )
        .unwrap();
        chat.paste("Revisão em andamento");
        chat.save_form(&mut db).unwrap();
        assert_eq!(chat.session.title, "Revisão em andamento");
        assert!(!chat.running.as_ref().unwrap().job.cancelling());
        assert_eq!(
            chat.history.read_session(&first_id).unwrap().summary.title,
            "Revisão em andamento"
        );
        chat.finish_all(&db).unwrap();
    }

    #[test]
    fn tabs_preserve_drafts_profile_options_and_close_without_deleting_history() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.choose();
        let first = chat.session.id.clone();
        chat.paste("Rascunho da primeira aba 界");
        chat.composer.cursor = 9;
        chat.scroll = 7;
        chat.follow = false;
        chat.run_form.fields[4].value = "111".into();
        chat.key(
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
            &mut db,
            &mut None,
        )
        .unwrap();
        assert_eq!(chat.tabs.len(), 2);
        assert_ne!(chat.session.id, first);
        chat.paste("Rascunho da segunda aba");
        chat.chosen.as_mut().unwrap().name = "another-profile".into();
        chat.run_form.fields[4].value = "222".into();
        chat.switch_tab(0);
        assert_eq!(chat.session.id, first);
        assert_eq!(chat.composer.value, "Rascunho da primeira aba 界");
        assert_eq!(chat.composer.cursor, 9);
        assert_eq!(chat.scroll, 7);
        assert!(!chat.follow);
        assert_eq!(chat.chosen.as_ref().unwrap().name, "selected");
        assert_eq!(chat.run_form.fields[4].value, "111");
        chat.switch_tab(1);
        assert_eq!(chat.composer.value, "Rascunho da segunda aba");
        assert_eq!(chat.chosen.as_ref().unwrap().name, "another-profile");
        assert_eq!(chat.run_form.fields[4].value, "222");
        let closed = chat.session.id.clone();
        let turn = chat
            .history
            .start_turn(&closed, "Entrega salva", "another-profile", "Concluído")
            .unwrap();
        chat.history
            .update_turn(
                &closed,
                &turn,
                &["Resultado".into()],
                "Concluído",
                None,
                true,
            )
            .unwrap();
        chat.tab_action(TabAction::Close).unwrap();
        assert_eq!(chat.tabs.len(), 1);
        assert_eq!(chat.session.id, first);
        assert!(
            chat.history
                .sessions(dir.path())
                .unwrap()
                .iter()
                .any(|session| session.id == closed && session.ended_at.is_some())
        );
        chat.tab_action(TabAction::Close).unwrap();
        assert_eq!(chat.tabs.len(), 1);
        assert!(chat.turns.is_empty());
        assert_ne!(chat.session.id, first);
        assert!(db.executions().unwrap().is_empty());
    }

    #[test]
    fn rendered_mouse_targets_select_profiles_tabs_options_and_unicode_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.frame(124, 34);
        click(&mut chat, &mut db, 6, 10);
        assert!(matches!(chat.stage, Stage::Chat));
        chat.paste("a界b");
        chat.frame(124, 34);
        let input = chat
            .clicks
            .iter()
            .find(|(_, action)| matches!(action, ClickAction::Composer { .. }))
            .unwrap()
            .0;
        click(&mut chat, &mut db, input.x + 3, input.y);
        chat.paste("X");
        assert_eq!(chat.composer.value, "a界Xb");
        chat.frame(124, 34);
        let settings = chat
            .clicks
            .iter()
            .find(|(_, action)| matches!(action, ClickAction::Tab(TabAction::Settings)))
            .unwrap()
            .0;
        click(&mut chat, &mut db, settings.x, settings.y);
        assert!(chat.settings_open);
        chat.frame(124, 34);
        let options = chat
            .clicks
            .iter()
            .find(|(_, action)| matches!(action, ClickAction::Setting(2)))
            .unwrap()
            .0;
        click(&mut chat, &mut db, options.x, options.y);
        assert!(matches!(chat.stage, Stage::Form(_)));
        assert_eq!(chat.composer.value, "a界Xb");
        chat.frame(124, 34);
        let field = chat
            .clicks
            .iter()
            .find(|(_, action)| matches!(action, ClickAction::FormField(2, _)))
            .unwrap()
            .0;
        click(&mut chat, &mut db, field.x + 3, field.y + 1);
        assert!(matches!(&chat.stage, Stage::Form(editor) if editor.selected == 2));
        let original = chat.session.id.clone();
        if let Stage::Form(editor) = &mut chat.stage {
            editor.inputs[2].value = "321".into();
        }
        chat.frame(124, 34);
        let new = chat
            .clicks
            .iter()
            .find(|(_, action)| matches!(action, ClickAction::Tab(TabAction::New)))
            .unwrap()
            .0;
        click(&mut chat, &mut db, new.x, new.y);
        assert_eq!(chat.active_tab, 1);
        chat.frame(64, 20);
        assert!(
            chat.clicks
                .iter()
                .all(|(rect, _)| rect.x + rect.width <= 64 && rect.y + rect.height <= 20)
        );
        let first = chat
            .clicks
            .iter()
            .find(|(_, action)| matches!(action, ClickAction::Tab(TabAction::Next)))
            .unwrap()
            .0;
        click(&mut chat, &mut db, first.x, first.y);
        assert_eq!(chat.composer.value, "a界Xb");
        assert!(matches!(&chat.stage, Stage::Form(editor) if editor.selected == 2));
        chat.switch_tab(1);
        chat.load_session(&original).unwrap();
        assert!(matches!(&chat.stage, Stage::Form(editor) if editor.inputs[2].value == "321"));
        chat.frame(40, 12);
        assert!(chat.clicks.is_empty());
        click(&mut chat, &mut db, new.x, new.y);
        assert_eq!(chat.tabs.len(), 2);
    }

    #[cfg(unix)]
    fn fake_tab_job(chat: &mut Chat, title: &str, script: &str) {
        let id = chat
            .history
            .start_turn(&chat.session.id, title, "selected", "Trabalhando…")
            .unwrap();
        let job = Job::start(
            Path::new("/bin/sh"),
            &["-c".into(), script.into()],
            &chat.cwd,
            None,
        )
        .unwrap();
        chat.turns.push(Turn {
            id,
            prompt: title.into(),
            profile: "selected".into(),
            lines: vec![],
            status: "Trabalhando…".into(),
            execution: None,
            finished: false,
        });
        chat.running = Some(Running {
            job,
            approval_view: None,
            preview: false,
            turn: chat.turns.len() - 1,
            emitted: None,
            last_metrics: Instant::now(),
            last_history: Instant::now(),
        });
    }

    #[test]
    #[cfg(unix)]
    fn update_is_discoverable_and_rejects_arguments_and_busy_tabs() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.composer.value = "/up".into();
        assert!(chat.suggestions().iter().any(|(cmd, _)| *cmd == "/update"));
        chat.composer.value = "/help".into();
        chat.command(&mut db, &mut None).unwrap();
        assert!(chat.note.iter().any(|line| line.contains("/update")));
        chat.composer.value = "/update unexpected".into();
        assert!(
            chat.command(&mut db, &mut None)
                .unwrap_err()
                .to_string()
                .contains("não aceita argumentos")
        );
        chat.choose();
        fake_tab_job(&mut chat, "Pedido ativo", "sleep 0.1");
        chat.new_session().unwrap();
        chat.composer.value = "/update".into();
        assert!(
            chat.command(&mut db, &mut None)
                .unwrap_err()
                .to_string()
                .contains("pedidos em execução")
        );
        chat.finish_all(&db).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn background_jobs_keep_their_session_and_finish_without_interrupting_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, _) = fixture(dir.path());
        let db = Db::open(&chat.options.db).unwrap();
        chat.choose();
        let first = chat.session.id.clone();
        fake_tab_job(
            &mut chat,
            "Primeiro pedido",
            "printf 'resultado-aba-A\\n'; sleep 0.1",
        );
        assert!(chat.tab_action(TabAction::Close).is_err());
        chat.new_session().unwrap();
        let second = chat.session.id.clone();
        chat.stage = Stage::Profiles;
        let mut db = db;
        assert!(!chat.key(key(KeyCode::F(2)), &mut db, &mut None).unwrap());
        assert!(chat.any_running());
        assert!(chat.notice.contains("pedidos em execução"));
        chat.stage = Stage::Chat;
        fake_tab_job(
            &mut chat,
            "Segundo pedido",
            "printf 'resultado-aba-B\\n'; sleep 0.1",
        );
        chat.load_session(&first).unwrap();
        assert_eq!(
            chat.tabs.len(),
            2,
            "an open session must be focused, never duplicated"
        );
        assert!(chat.running.is_some());
        assert!(
            chat.history.read_session(&first).unwrap().turns[0]
                .ended_at
                .is_none()
        );
        let mut outsider = ChatHistory::open(&chat.cwd, &chat.options.db).unwrap();
        assert!(
            outsider.load_session(&second, dir.path()).is_err(),
            "parked tabs must retain ownership"
        );
        chat.switch_tab(1);
        let until = Instant::now() + Duration::from_secs(3);
        let mut input_refresh_needed = false;
        while chat.any_running() && Instant::now() < until {
            input_refresh_needed |= chat.poll_all(&db);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!chat.any_running());
        assert!(input_refresh_needed);
        assert!(
            !chat.poll_all(&db),
            "idle frames must not retrigger completion"
        );
        assert!(
            chat.turns[0]
                .lines
                .iter()
                .any(|line| line == "resultado-aba-B")
        );
        let first_tab = chat.tabs[0].as_ref().unwrap();
        assert!(first_tab.unread && first_tab.turns[0].finished);
        assert!(
            first_tab.turns[0]
                .lines
                .iter()
                .any(|line| line == "resultado-aba-A")
        );
        assert!(
            chat.history.read_session(&first).unwrap().turns[0]
                .ended_at
                .is_some()
        );
        chat.switch_tab(0);
        assert!(!chat.unread);
        chat.finish_all(&db).unwrap();
        outsider.load_session(&second, dir.path()).unwrap();
    }

    fn compiled_profile(dir: &Path, name: &str, count: usize, delegation: &str) {
        use crate::profiles::{self, AgentSpec, Profile, TeamSpec};
        let all = dir.join("providers/project/all");
        std::fs::write(all.join(format!("{name}.png")), b"image fixture").unwrap();
        let agent = |role: String| AgentSpec {
            provider: None,
            role,
            model: "gpt-6-astra".into(),
            effort: "medium".into(),
            purpose: "Executar a finalidade configurada".into(),
            when: "Quando necessário".into(),
        };
        let profile = Profile {
            schema_version: 1,
            source_image: format!("{name}.png"),
            source_sha256: profiles::digest(b"image fixture"),
            generated_by: "fixture".into(),
            generated_at: "2026-09-12T12:00:00Z".parse().unwrap(),
            team: TeamSpec {
                name: name.into(),
                provider: "openai".into(),
                orchestrator: agent("root".into()),
                agents: (1..=count)
                    .map(|index| agent(format!("role_{index}")))
                    .collect(),
                delegation: delegation.into(),
                integration: "Integração final configurada".into(),
                notes: String::new(),
            },
        };
        std::fs::write(
            all.join(format!("{name}.md")),
            profiles::markdown(&profile).unwrap(),
        )
        .unwrap();
    }

    fn completed_measured_turn(chat: &mut Chat, db: &mut Db, title: &str, tokens: u64) -> String {
        let team = chat.chosen.as_ref().unwrap().team.clone().unwrap();
        let job = Execution {
            id: uuid::Uuid::new_v4().to_string(),
            kind: "task".into(),
            project: chat.cwd.to_string_lossy().into(),
            profile_name: team.name.clone(),
            profile_sha256: "fixture".into(),
            profile_markdown: String::new(),
            planned_stack: team,
            observed_stack: None,
            assistant_provider: "openai".into(),
            assistant_model: "fixture".into(),
            assistant_effort: "medium".into(),
            execution_client: Backend::Codex,
            observed_model: None,
            reported_cost_usd: None,
            max_agents: 4,
            sandbox: "read-only".into(),
            prompt_sha256: "fixture".into(),
            prompt_chars: title.chars().count(),
            benchmark: String::new(),
            started_at: Utc::now(),
            ended_at: Some(Utc::now()),
            wall_ms: Some(1000),
            status: "completed".into(),
            root_id: None,
            cli_session_id: None,
            reported_tokens: Some(crate::model::Tokens {
                input_tokens: tokens,
                ..Default::default()
            }),
            metrics: None,
            coverage: "cli_tree".into(),
            error: None,
            feedback: None,
        };
        db.save_execution(&job).unwrap();
        let id = chat
            .history
            .start_turn(&chat.session.id, title, "selected", "Concluído")
            .unwrap();
        let lines = vec![format!("Resposta: {title}")];
        chat.history
            .update_turn(&chat.session.id, &id, &lines, "Concluído", Some(&job), true)
            .unwrap();
        let execution_id = job.id.clone();
        chat.turns.push(Turn {
            id,
            prompt: title.into(),
            profile: "selected".into(),
            lines,
            status: "Concluído".into(),
            execution: Some(job),
            finished: true,
        });
        execution_id
    }

    #[test]
    fn widgets_rate_the_selected_prompt_keep_totals_after_clear_and_refresh_external_feedback() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        compiled_profile(dir.path(), "selected", 0, "on_demand");
        chat.refresh_profiles();
        chat.choose();
        let first_id = completed_measured_turn(&mut chat, &mut db, "Primeira entrega", 100);
        let second_id = completed_measured_turn(&mut chat, &mut db, "Segunda entrega", 300);
        let stats = chat.conversation_stats();
        assert_eq!(stats.total_tokens, Some(400));
        assert_eq!(stats.score, None);
        chat.frame(124, 34);
        let previous = chat
            .clicks
            .iter()
            .find(|(_, action)| matches!(action, ClickAction::Widget(WidgetAction::Previous)))
            .unwrap()
            .0;
        click(&mut chat, &mut db, previous.x, previous.y);
        assert_eq!(chat.metric_pick, Some(0));
        chat.frame(124, 34);
        let rate = chat
            .clicks
            .iter()
            .find(|(_, action)| matches!(action, ClickAction::Widget(WidgetAction::Rate)))
            .unwrap()
            .0;
        click(&mut chat, &mut db, rate.x, rate.y);
        assert!(matches!(chat.stage, Stage::Feedback(_)));
        let frame = chat.frame(124, 34);
        assert!(
            frame
                .spans
                .iter()
                .any(|span| span.text.contains("Resposta: Primeira entrega"))
        );
        assert!(
            frame
                .spans
                .iter()
                .any(|span| span.text.contains("Segunda entrega"))
        );
        let submit = chat
            .clicks
            .iter()
            .find(|(_, action)| matches!(action, ClickAction::FeedbackSubmit))
            .unwrap()
            .0;
        click(&mut chat, &mut db, submit.x, submit.y);
        let Stage::Form(editor) = &mut chat.stage else {
            panic!("rating should open a form")
        };
        assert_eq!(
            editor.form.fields[0].value, first_id,
            "the selected prompt must be evaluated, not the latest"
        );
        editor.inputs[0].value = "0,4".into();
        editor.inputs[1].value = "2".into();
        editor.inputs[2].value = "Entrega parcial informada pelo cliente".into();
        chat.save_form(&mut db).unwrap();
        assert_eq!(chat.conversation_stats().score, Some(4.0));
        assert!(
            execution(&db, &second_id)
                .unwrap()
                .unwrap()
                .feedback
                .is_none()
        );
        let mut external = Db::open(&chat.options.db).unwrap();
        workflow::save_feedback(
            &mut external,
            &second_id,
            Feedback {
                delivered: 1.0,
                speed: 5,
                note: String::new(),
                recorded_at: Utc::now(),
            },
        )
        .unwrap();
        chat.metrics_checked = Instant::now() - Duration::from_secs(2);
        chat.poll_all(&db);
        assert_eq!(chat.conversation_stats().score, Some(7.0));
        assert_eq!(chat.conversation_stats().rated, 2);
        chat.paste("/clear");
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        assert_eq!(chat.hidden_turns, 2);
        assert_eq!(chat.conversation_stats().total_tokens, Some(400));
        assert_eq!(chat.conversation_stats().score, Some(7.0));
        chat.new_session().unwrap();
        assert_eq!(chat.conversation_stats().total_tokens, Some(0));
        assert_eq!(chat.conversation_stats().score, None);
        chat.switch_tab(0);
        assert_eq!(chat.metric_pick, Some(0));
        assert_eq!(chat.hidden_turns, 2);
        chat.key(key(KeyCode::F(7)), &mut db, &mut None).unwrap();
        let details = chat.note.join("\n");
        assert!(details.contains("100 tokens") && details.contains("300 tokens"));
        let id = chat.session.id.clone();
        chat.tab_action(TabAction::Close).unwrap();
        chat.load_session(&id).unwrap();
        chat.poll_all(&db);
        assert_eq!(chat.conversation_stats().total_tokens, Some(400));
        assert_eq!(
            chat.conversation_stats().score,
            Some(7.0),
            "reopening must hydrate newer feedback from the main database"
        );
        assert_eq!(chat.hidden_turns, 0);
    }

    #[test]
    fn feedback_picker_lists_every_turn_state_and_scrolls_the_complete_response() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        compiled_profile(dir.path(), "selected", 0, "on_demand");
        chat.refresh_profiles();
        chat.choose();
        let execution_id = completed_measured_turn(&mut chat, &mut db, "Pedido avaliável", 100);
        chat.turns[0].lines = (1..=24)
            .map(|line| format!("Conteúdo completo da resposta · linha {line}"))
            .collect();
        chat.turns.push(Turn {
            id: "turn-without-execution".into(),
            prompt: "Pedido concluído sem registro".into(),
            profile: "selected".into(),
            lines: vec!["Resposta preservada sem execução".into()],
            status: "Concluído".into(),
            execution: None,
            finished: true,
        });
        chat.turns.push(Turn {
            id: "running-turn".into(),
            prompt: "Pedido ainda em andamento".into(),
            profile: "selected".into(),
            lines: vec!["Resposta parcial em andamento".into()],
            status: "Em andamento".into(),
            execution: None,
            finished: false,
        });
        chat.metric_pick = Some(0);
        chat.widget_action(WidgetAction::Rate, &db).unwrap();

        let frame = chat.frame(124, 40);
        let content = frame
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(content.contains("Pedido avaliável"));
        assert!(content.contains("Pedido concluído sem registro"));
        assert!(content.contains("Pedido ainda em andamento"));
        assert!(content.contains("Disponível para avaliar"));
        assert!(content.contains("Sem registro de execução · avaliação indisponível"));
        assert!(content.contains("Em andamento · sem registro de execução"));
        assert!(content.contains("Conteúdo completo da resposta · linha 1"));
        assert!(!content.contains("Conteúdo completo da resposta · linha 24"));

        chat.key(key(KeyCode::End), &mut db, &mut None).unwrap();
        let frame = chat.frame(124, 40);
        assert!(frame.spans.iter().any(|span| {
            span.text
                .contains("Conteúdo completo da resposta · linha 24")
        }));

        chat.key(key(KeyCode::Down), &mut db, &mut None).unwrap();
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        assert!(matches!(chat.stage, Stage::Feedback(_)));
        assert!(chat.notice.contains("ainda não tem uma execução concluída"));

        if let Stage::Feedback(picker) = &mut chat.stage {
            picker.selected = 0;
        }
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        assert!(matches!(
            &chat.stage,
            Stage::Form(editor) if editor.form.fields[0].value == execution_id
        ));
    }

    #[test]
    fn transcript_omits_rating_controls_and_feedback_marker() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        compiled_profile(dir.path(), "selected", 0, "on_demand");
        chat.refresh_profiles();
        chat.choose();
        let id = completed_measured_turn(&mut chat, &mut db, "Resposta", 100);
        workflow::save_feedback(
            &mut db,
            &id,
            Feedback {
                delivered: 1.0,
                speed: 4,
                note: "Contexto preservado".into(),
                recorded_at: Utc::now(),
            },
        )
        .unwrap();
        chat.refresh_execution_metrics(&db, true);
        let frame = chat.frame(124, 34);
        assert!(
            frame
                .spans
                .iter()
                .any(|span| span.text == "NOTA DA CONVERSA")
        );
        assert!(frame.spans.iter().any(|span| span.text == "[Avaliar]"));
        assert!(!frame.spans.iter().any(|span| {
            matches!(
                span.text.as_str(),
                "Avalie:"
                    | "[Excelente]"
                    | "[Excelente*]"
                    | "[Bom]"
                    | "[Bom*]"
                    | "[Mediano]"
                    | "[Mediano*]"
                    | "[Ruim]"
                    | "[Ruim*]"
            )
        }));
        let job = execution(&db, &id).unwrap().unwrap();
        assert!(!metrics(&job).contains("avaliada"));
    }

    #[test]
    fn widgets_precede_tabs_and_keep_composer_clear_on_resize() {
        use unicode_width::UnicodeWidthStr;
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        compiled_profile(dir.path(), "selected", 0, "on_demand");
        chat.refresh_profiles();
        chat.choose();
        completed_measured_turn(&mut chat, &mut db, "Resumo", 900);
        for (width, height) in [(64, 20), (80, 24), (110, 28), (124, 34), (160, 40)] {
            chat.composer = Composer::default();
            for text in ["", "linha 1\nlinha 2\nlinha 3\nlinha 4\nlinha 5", "/"] {
                chat.composer.value = text.into();
                chat.composer.cursor = text.len();
                let frame = chat.frame(width, height);
                assert!(
                    frame
                        .spans
                        .iter()
                        .any(|span| span.y == if height < 28 { 4 } else { 6 }
                            && span.text.contains("Nova conversa"))
                );
                assert!(
                    frame
                        .spans
                        .iter()
                        .any(|span| span.y == 0 && span.text.contains("CONVERSA"))
                );
                let tokens_header = frame
                    .spans
                    .iter()
                    .find(|span| span.y == 0 && span.text.contains("TOKENS POR PEDIDO"))
                    .expect("per-prompt token widget remains visible after resize");
                assert!(frame.spans.iter().any(|span| span.y == 1
                    && span.x >= tokens_header.x
                    && span.text.contains("900")));
                assert!(
                    frame
                        .spans
                        .iter()
                        .all(|span| span.x + span.text.width() <= width as usize
                            && span.y < height as usize)
                );
                let input_top = frame
                    .spans
                    .iter()
                    .find(|span| span.x == 2 && span.text.starts_with("───"))
                    .unwrap()
                    .y;
                for (rect, action) in &chat.clicks {
                    assert!(
                        rect.x + rect.width <= width as usize
                            && rect.y + rect.height <= height as usize
                    );
                    if matches!(action, ClickAction::Widget(_)) {
                        let tab_y = if height < 28 { 4 } else { 6 };
                        assert!(rect.y + rect.height <= tab_y && rect.y + rect.height <= input_top);
                    }
                }
            }
        }
    }

    #[test]
    fn startup_never_autoaccepts_even_single_default_and_selecting_does_not_compile() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        assert!(matches!(chat.stage, Stage::Profiles));
        assert!(chat.chosen.is_none());
        assert!(chat.send("pedido".into(), false).is_err());
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        assert!(matches!(chat.stage, Stage::Chat));
        assert_eq!(chat.chosen.as_ref().unwrap().name, "selected");
        assert_eq!(chat.run_form.fields[1].value, "selected");
        assert!(chat.running.is_none());
        assert!(db.executions().unwrap().is_empty());
        assert!(
            !dir.path()
                .join("providers/project/all/selected.md")
                .exists()
        );
    }

    #[test]
    fn agent_detail_keeps_drafts_separate_from_forms_tabs_and_main_send() {
        use crate::activity::AgentActivity;
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.choose();
        chat.activity.agents.push(AgentActivity {
            id: "child-1".into(),
            parent_id: Some("root".into()),
            title: "Revisar testes".into(),
            model: Some("Sol".into()),
            effort: Some("high".into()),
            preview: "cargo test --lib".into(),
            status: AgentStatus::Running,
            started_at: None,
            finished_at: None,
        });
        chat.composer.value = "pedido principal\nlinha 2\nlinha 3\nlinha 4\nlinha 5".into();
        chat.composer.cursor = chat.composer.value.len();
        let original = chat.composer.value.clone();
        chat.frame(148, 38);
        let card = chat
            .clicks
            .iter()
            .find(|(_, action)| matches!(action, ClickAction::Agent(id) if id == "child-1"))
            .unwrap()
            .0;
        click(&mut chat, &mut db, card.x, card.y);
        chat.paste("orientação específica 界");
        let draft = chat.agent_view.drafts["child-1"].value.clone();
        assert_eq!(chat.composer.value, original);
        let frame = chat.frame(64, 20);
        assert!(
            frame
                .spans
                .iter()
                .any(|span| span.text.contains("cargo test"))
        );
        assert!(
            frame
                .spans
                .iter()
                .any(|span| span.text.contains("orientação específica"))
        );
        assert!(frame.caret.is_some());
        assert!(!chat.clicks.iter().any(|(_, action)| matches!(
            action,
            ClickAction::Submit
                | ClickAction::AgentDetail(DetailAction::Send | DetailAction::InterruptRedirect)
        )));
        chat.tab_action(TabAction::New).unwrap();
        assert!(chat.agent_view.selected.is_none());
        chat.tab_action(TabAction::Select(0)).unwrap();
        assert_eq!(chat.agent_view.drafts["child-1"].value, draft);
        chat.edit_form(Action::Run).unwrap();
        chat.paste("valor do formulário");
        assert_eq!(chat.agent_view.drafts["child-1"].value, draft);
        assert!(
            matches!(&chat.stage, Stage::Form(editor) if editor.inputs[0].value.contains("valor do formulário"))
        );
        chat.stage = Stage::Chat;
        chat.composer.value = "/help".into();
        chat.frame(148, 38);
        let submit = chat
            .clicks
            .iter()
            .find(|(_, action)| matches!(action, ClickAction::Submit))
            .unwrap()
            .0;
        click(&mut chat, &mut db, submit.x, submit.y);
        assert!(chat.note.iter().any(|line| line.contains("/settings")));
        assert_eq!(chat.agent_view.drafts["child-1"].value, draft);
        chat.agent_view.focused = true;
        chat.key(key(KeyCode::Esc), &mut db, &mut None).unwrap();
        assert!(chat.agent_view.selected.is_none());
    }

    #[test]
    fn leaving_agent_opened_from_expanded_team_restores_main_chat() {
        use crate::activity::AgentActivity;
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        compiled_profile(dir.path(), "selected", 1, "on_demand");
        chat.refresh_profiles();
        chat.choose();
        chat.activity.agents.push(AgentActivity {
            id: "child-1".into(),
            parent_id: Some("root".into()),
            title: "Revisar retorno".into(),
            model: Some("Sol".into()),
            effort: Some("high".into()),
            preview: "Verificando navegação".into(),
            status: AgentStatus::Running,
            started_at: None,
            finished_at: None,
        });
        completed_measured_turn(&mut chat, &mut db, "Histórico principal visível", 100);
        chat.composer.value = "Rascunho principal preservado".into();
        chat.composer.cursor = chat.composer.value.len();

        for (width, height) in [(148, 38), (64, 20)] {
            chat.key(key(KeyCode::F(4)), &mut db, &mut None).unwrap();
            assert!(chat.agents_expanded);
            chat.frame(width, height);
            let card = chat
                .clicks
                .iter()
                .find(|(_, action)| matches!(action, ClickAction::Agent(id) if id == "child-1"))
                .unwrap()
                .0;
            click(&mut chat, &mut db, card.x, card.y);
            assert_eq!(chat.agent_view.selected.as_deref(), Some("child-1"));
            chat.paste("Orientação ainda preservada");
            let agent_draft = chat.agent_view.drafts["child-1"].value.clone();

            chat.frame(width, height);
            let back = chat
                .clicks
                .iter()
                .find(|(_, action)| matches!(action, ClickAction::AgentDetail(DetailAction::Back)))
                .unwrap()
                .0;
            click(&mut chat, &mut db, back.x, back.y);

            assert!(chat.agent_view.selected.is_none());
            assert!(!chat.agent_view.focused);
            assert!(!chat.agents_expanded);
            assert_eq!(chat.agent_view.drafts["child-1"].value, agent_draft);
            let restored = chat.frame(width, height);
            assert!(
                restored
                    .spans
                    .iter()
                    .any(|span| span.text.contains("Rascunho principal preservado"))
            );
            assert!(
                restored
                    .spans
                    .iter()
                    .any(|span| span.text.contains("Resposta: Histórico principal visível"))
            );
            assert!(
                chat.clicks
                    .iter()
                    .any(|(_, action)| matches!(action, ClickAction::Composer { .. }))
            );
            assert!(
                chat.clicks
                    .iter()
                    .any(|(_, action)| matches!(action, ClickAction::Submit))
            );
        }

        chat.key(key(KeyCode::F(4)), &mut db, &mut None).unwrap();
        chat.frame(64, 20);
        let card = chat
            .clicks
            .iter()
            .find(|(_, action)| matches!(action, ClickAction::Agent(id) if id == "child-1"))
            .unwrap()
            .0;
        click(&mut chat, &mut db, card.x, card.y);
        chat.key(key(KeyCode::Esc), &mut db, &mut None).unwrap();
        assert!(chat.agent_view.selected.is_none());
        assert!(!chat.agent_view.focused);
        assert!(!chat.agents_expanded);
    }

    #[test]
    fn settings_navigation_exposes_the_chosen_page_and_keeps_drafts() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        compiled_profile(dir.path(), "selected", 1, "on_demand");
        chat.refresh_profiles();
        chat.choose();
        completed_measured_turn(&mut chat, &mut db, "Pedido concluído", 100);
        chat.agent_view.selected = Some("child".into());
        chat.agent_view.focused = true;
        chat.composer.value = "Rascunho principal".into();
        chat.tab_action(TabAction::Settings).unwrap();
        chat.setting(1, &mut db, &mut None).unwrap();
        assert!(!chat.settings_open && chat.agent_view.selected.is_none());
        assert_eq!(chat.composer.value, "Rascunho principal");
        assert!(chat.note.iter().any(|line| line == "EQUIPE CONFIGURADA"));
        chat.tab_action(TabAction::Settings).unwrap();
        chat.widget_action(WidgetAction::Rate, &db).unwrap();
        assert!(!chat.settings_open && matches!(chat.stage, Stage::Feedback(_)));
        chat.tab_action(TabAction::Settings).unwrap();
        chat.key(key(KeyCode::F(6)), &mut db, &mut None).unwrap();
        assert!(!chat.settings_open);
        chat.tab_action(TabAction::Settings).unwrap();
        chat.tab_action(TabAction::Close).unwrap();
        assert_eq!(chat.tabs.len(), 1);
    }

    #[test]
    #[cfg(unix)]
    fn unavailable_agent_control_preserves_job_draft_and_metrics() {
        use crate::activity::AgentActivity;
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.choose();
        fake_tab_job(&mut chat, "Pedido ativo", "sleep 2");
        chat.activity.agents.push(AgentActivity {
            id: "child".into(),
            parent_id: Some("root".into()),
            title: "Tarefa".into(),
            model: None,
            effort: None,
            preview: "Trabalhando".into(),
            status: AgentStatus::Running,
            started_at: Some(Utc::now() - chrono::Duration::seconds(10)),
            finished_at: None,
        });
        chat.open_agent("child".into());
        chat.paste("Mude o foco da revisão");
        assert!(chat.send_agent(true).is_err());
        assert!(!chat.running.as_ref().unwrap().job.cancelling());
        assert!(chat.agent_view.messages.is_empty());
        assert_eq!(
            chat.agent_view.drafts["child"].value,
            "Mude o foco da revisão"
        );
        chat.key(key(KeyCode::Esc), &mut db, &mut None).unwrap();
        assert!(!chat.running.as_ref().unwrap().job.cancelling());
        chat.tab_action(TabAction::Settings).unwrap();
        assert!(chat.setting(2, &mut db, &mut None).is_err());
        assert!(chat.settings_open);
        chat.finish_all(&db).unwrap();
        let agent = &chat.activity.agents[0];
        let frozen = agent.elapsed_at(Utc::now()).unwrap();
        assert_eq!(agent.status, AgentStatus::Cancelled);
        assert_eq!(
            agent.elapsed_at(Utc::now() + chrono::Duration::hours(1)),
            Some(frozen)
        );
        let saved = chat.history.activity(&chat.turns[0].id).unwrap().unwrap();
        assert_eq!(saved.agents[0].finished_at, agent.finished_at);
        assert!(db.executions().unwrap().is_empty());
    }

    #[test]
    fn recovered_agent_without_end_does_not_accumulate_offline_time() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, _) = fixture(dir.path());
        let session = chat.history.create_session(&chat.cwd).unwrap();
        let turn = chat
            .history
            .start_turn(&session.id, "Pedido", "selected", "Executando")
            .unwrap();
        let snapshot = ActivitySnapshot {
            agents: vec![crate::activity::AgentActivity {
                id: "recovered-child".into(),
                parent_id: None,
                title: "Tarefa interrompida".into(),
                model: None,
                effort: None,
                preview: "Última atividade".into(),
                status: AgentStatus::Running,
                started_at: Some(Utc::now() - chrono::Duration::days(1)),
                finished_at: None,
            }],
            ..ActivitySnapshot::default()
        };
        chat.history
            .save_activity(&session.id, &turn, &snapshot)
            .unwrap();
        chat.load_session(&session.id).unwrap();
        assert!(chat.running.is_none());
        let agent = &chat.activity.agents[0];
        assert_eq!(agent.status, AgentStatus::Unknown);
        assert!(agent.started_at.is_some());
        assert!(agent.elapsed_at(Utc::now()).is_none());
    }

    #[test]
    fn newline_shortcuts_edit_main_agent_and_multiline_form_drafts_without_sending() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        compiled_profile(dir.path(), "selected", 0, "on_demand");
        chat.refresh_profiles();
        chat.choose();
        let shortcuts = [
            KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        ];
        for shortcut in shortcuts {
            chat.composer = Composer::default();
            chat.paste("a界b");
            chat.composer.cursor = 4;
            chat.key(shortcut, &mut db, &mut None).unwrap();
            assert_eq!(chat.composer.value, "a界\nb");
            assert_eq!(chat.composer.cursor, 5);
        }
        chat.agent_view.selected = Some("child".into());
        chat.agent_view.focused = true;
        for shortcut in shortcuts {
            chat.key(shortcut, &mut db, &mut None).unwrap();
        }
        assert_eq!(chat.agent_view.drafts["child"].value, "\n\n\n");
        chat.agent_view.selected = None;
        completed_measured_turn(&mut chat, &mut db, "Resultado", 42);
        chat.edit_form(Action::Feedback).unwrap();
        let Stage::Form(editor) = &mut chat.stage else {
            panic!("form expected")
        };
        editor.selected = editor
            .indices
            .iter()
            .position(|index| editor.form.fields[*index].multiline)
            .unwrap();
        let selected = editor.selected;
        editor.inputs[selected] = Composer::default();
        for shortcut in shortcuts {
            chat.key(shortcut, &mut db, &mut None).unwrap();
        }
        let Stage::Form(editor) = &chat.stage else {
            panic!("form must remain open")
        };
        assert_eq!(editor.selected, selected);
        assert_eq!(editor.inputs[selected].value, "\n\n\n");
        assert!(!chat.any_running());
        assert_eq!(db.executions().unwrap().len(), 1);
        assert_eq!(chat.turns.len(), 1);
        assert!(!is_newline(key(KeyCode::Enter)));
    }

    #[test]
    fn selection_mode_protects_draft_from_submit_copy_and_paste_until_explicit_exit() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.choose();
        chat.paste("/exit");
        chat.key(key(KeyCode::F(8)), &mut db, &mut None).unwrap();
        assert!(chat.selecting_text);
        for input in [
            key(KeyCode::Enter),
            key(KeyCode::Char('x')),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
        ] {
            assert!(!chat.key(input, &mut db, &mut None).unwrap());
        }
        chat.paste("unexpected clipboard");
        assert_eq!(chat.composer.value, "/exit");
        assert_eq!(chat.tabs.len(), 1);
        for (width, height) in [(64, 20), (80, 24)] {
            let frame = chat.frame(width, height);
            assert!(frame.caret.is_none());
            assert!(
                frame
                    .spans
                    .iter()
                    .any(|span| span.text.contains("COPIAR:") && span.text.contains("F8/Esc"))
            );
        }
        chat.key(
            KeyEvent::new_with_kind(KeyCode::F(8), KeyModifiers::NONE, KeyEventKind::Repeat),
            &mut db,
            &mut None,
        )
        .unwrap();
        assert!(chat.selecting_text);
        chat.key(key(KeyCode::Esc), &mut db, &mut None).unwrap();
        assert!(!chat.selecting_text);
        assert!(chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap());
        chat.key(key(KeyCode::F(8)), &mut db, &mut None).unwrap();
        chat.key(key(KeyCode::F(8)), &mut db, &mut None).unwrap();
        assert!(!chat.selecting_text);
    }

    #[test]
    #[cfg(unix)]
    fn selection_mode_keeps_background_requests_running_and_polling() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.choose();
        fake_tab_job(
            &mut chat,
            "Copiar durante execução",
            "sleep 0.05; printf 'concluido\\n'",
        );
        chat.key(key(KeyCode::F(8)), &mut db, &mut None).unwrap();
        chat.key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut db,
            &mut None,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while chat.any_running() && Instant::now() < deadline {
            chat.poll_all(&db);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!chat.any_running());
        assert!(chat.selecting_text);
        assert!(
            chat.turns[0]
                .lines
                .iter()
                .any(|line| line.contains("concluido"))
        );
    }

    #[test]
    fn newline_exit_and_profile_switch_are_local_commands_not_requests() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.choose();
        chat.paste("ação\tUnicode");
        chat.key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
            &mut db,
            &mut None,
        )
        .unwrap();
        assert_eq!(chat.composer.value, "ação\tUnicode\n");
        assert!(chat.running.is_none());
        chat.composer = Composer::default();
        chat.paste("/profile");
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        assert!(matches!(chat.stage, Stage::Profiles));
        chat.key(key(KeyCode::Esc), &mut db, &mut None).unwrap();
        assert!(matches!(chat.stage, Stage::Chat));
        chat.paste("/exit  ");
        assert!(chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap());
        assert!(db.executions().unwrap().is_empty());
    }

    #[test]
    fn clear_new_and_sessions_preserve_and_reopen_project_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.choose();
        let first_session = chat.session.id.clone();
        let turn_id = chat
            .history
            .start_turn(
                &first_session,
                "Pedido persistido",
                "selected",
                "Trabalhando…",
            )
            .unwrap();
        chat.history
            .update_turn(
                &first_session,
                &turn_id,
                &["Resposta persistida".into()],
                "Concluído",
                None,
                true,
            )
            .unwrap();
        chat.turns.push(Turn {
            id: turn_id,
            prompt: "Pedido persistido".into(),
            profile: "selected".into(),
            lines: vec!["Resposta persistida".into()],
            status: "Concluído".into(),
            execution: None,
            finished: true,
        });

        chat.paste("/clear");
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        assert_eq!(chat.hidden_turns, chat.turns.len());
        assert!(chat.notice.contains("/sessions"));
        chat.paste("/sessions");
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        assert!(matches!(chat.stage, Stage::Sessions(_)));
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        assert_eq!(chat.turns[0].prompt, "Pedido persistido");
        assert_eq!(chat.turns[0].lines, ["Resposta persistida"]);

        chat.paste("/new");
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        assert_ne!(chat.session.id, first_session);
        assert!(chat.turns.is_empty());
        chat.paste("/sessions");
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        chat.key(key(KeyCode::Down), &mut db, &mut None).unwrap();
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        assert_eq!(chat.session.id, first_session);
        assert_eq!(chat.turns[0].profile, "selected");
        assert_eq!(chat.chosen.as_ref().unwrap().name, "selected");
        assert!(db.executions().unwrap().is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn send_transports_session_history_after_clear_profile_change_new_and_reopen() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("fake-stackpulse");
        fs::write(&executable, r#"#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
path = Path(args[args.index('--chat-context') + 1])
context = path.read_text()
with Path('received.jsonl').open('a') as output:
    output.write(json.dumps({'context':json.loads(context) if context else None, 'prompt':args[-1], 'path':str(path), 'mode':path.stat().st_mode & 0o777, 'args':args}) + '\n')
print('ANSWER-BEGIN:' + args[-1])
if args[-1] == 'primeiro pedido':
    for i in range(350):
        print(str(i) + ':' + 'resposta extensa ' * 20)
print('ANSWER-END:' + args[-1])
"#).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let finish = |chat: &mut Chat, db: &Db| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while chat.running.is_some() {
                chat.poll(db);
                assert!(Instant::now() < deadline, "fake chat runner did not finish");
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(chat.turns.last().unwrap().status, "Concluído");
        };
        let captured = || -> Vec<serde_json::Value> {
            fs::read_to_string(dir.path().join("received.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        };
        let (mut chat, mut db) = fixture(dir.path());
        chat.choose();
        chat.executable = executable.clone();
        let first = chat.session.id.clone();
        chat.send("primeiro pedido".into(), false).unwrap();
        finish(&mut chat, &db);
        assert!(captured()[0]["context"].is_null());
        assert_eq!(
            chat.turns[0].lines.len(),
            352,
            "completed answers must outlive the UI tail"
        );
        chat.paste("/clear");
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        chat.chosen.as_mut().unwrap().name = "outra-equipe".into();
        chat.send("continuação única".into(), false).unwrap();
        finish(&mut chat, &db);
        let rows = captured();
        let context = &rows[1]["context"];
        assert_eq!(context["session_id"], first);
        assert_eq!(context["turns"].as_array().unwrap().len(), 1);
        assert_eq!(context["turns"][0]["user"], "primeiro pedido");
        assert_eq!(context["turns"][0]["profile"], "selected");
        assert!(
            context["turns"][0]["assistant"]
                .as_str()
                .unwrap()
                .starts_with("ANSWER-BEGIN:")
        );
        assert!(
            context["turns"][0]["assistant"]
                .as_str()
                .unwrap()
                .ends_with("ANSWER-END:primeiro pedido")
        );
        assert!(!context.to_string().contains("continuação única"));
        assert_eq!(
            chat.history.read_session(&first).unwrap().turns[1].prompt,
            "continuação única"
        );

        chat.new_session().unwrap();
        chat.send("pedido isolado".into(), false).unwrap();
        finish(&mut chat, &db);
        assert!(captured()[2]["context"].is_null());
        drop(chat);

        let (mut reopened, db) = fixture(dir.path());
        reopened.executable = executable;
        reopened.load_session(&first).unwrap();
        reopened.choose();
        // A literal separator is still the user's prompt, never an option slot.
        reopened.send("--".into(), false).unwrap();
        finish(&mut reopened, &db);
        let rows = captured();
        assert_eq!(rows[3]["prompt"], "--");
        let turns = rows[3]["context"]["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[1]["user"], "continuação única");
        assert_eq!(turns[1]["profile"], "outra-equipe");
        assert!(!rows[3]["context"].to_string().contains("pedido isolado"));
        for row in rows {
            assert_eq!(row["mode"], 0o600);
            assert!(!Path::new(row["path"].as_str().unwrap()).exists());
        }
    }

    #[test]
    #[cfg(unix)]
    fn previews_from_command_and_options_do_not_become_conversation_context() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, db) = fixture(dir.path());
        chat.choose();
        chat.executable = "/usr/bin/true".into();
        for explicit in [true, false] {
            chat.run_form.fields[5].value = if explicit { "não" } else { "sim" }.into();
            chat.send("pedido de prévia".into(), explicit).unwrap();
            assert!(chat.running.as_ref().unwrap().preview);
            let deadline = Instant::now() + Duration::from_secs(2);
            while chat.running.is_some() {
                chat.poll(&db);
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(chat.turns.last().unwrap().status, "Prévia · Concluído");
            assert!(chat.history.context(&chat.session.id).unwrap().is_empty());
        }
        chat.run_form.fields[5].value = "não".into();
        chat.run_form.fields[2].value = "--dry-run".into();
        chat.send("--dry-run".into(), false).unwrap();
        assert!(!chat.running.as_ref().unwrap().preview);
        let deadline = Instant::now() + Duration::from_secs(2);
        while chat.running.is_some() {
            chat.poll(&db);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let context: serde_json::Value =
            serde_json::from_str(&chat.history.context(&chat.session.id).unwrap()).unwrap();
        assert_eq!(context["turns"].as_array().unwrap().len(), 1);
        assert_eq!(context["turns"][0]["user"], "--dry-run");
    }

    #[test]
    fn configured_team_is_visible_read_only_scrollable_and_refreshes_with_profile_and_settings() {
        use unicode_width::UnicodeWidthStr;
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.choose();
        chat.settings_open = true;
        assert!(
            chat.frame(64, 20)
                .spans
                .iter()
                .any(|span| span.text.contains("aguardando extração"))
        );
        chat.settings_open = false;
        chat.paste("/team");
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        assert!(chat.note.join("\n").contains("Aguardando extração"));
        assert!(chat.running.is_none());
        assert!(
            !dir.path()
                .join("providers/project/all/selected.md")
                .exists()
        );

        compiled_profile(dir.path(), "selected", 32, "on_demand");
        let mut settings = Settings::read(&chat.options.config).unwrap();
        settings.max_agents = 7;
        settings.save(&chat.options.config).unwrap();
        let config_before = std::fs::read(&chat.options.config).unwrap();
        chat.turns.push(Turn {
            id: "historical-turn".into(),
            prompt: "Pedido anterior preservado".into(),
            profile: "selected".into(),
            lines: vec!["Conteúdo anterior".into(); 80],
            status: "Concluído".into(),
            execution: None,
            finished: true,
        });
        chat.paste("/team");
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        assert!(chat.note.join("\n").contains("até 7 subagentes"));
        assert!(chat.note.join("\n").contains("32. role_32:"));
        assert!(chat.activity.agents.is_empty());
        let frame = chat.frame(64, 20);
        assert!(
            frame
                .spans
                .iter()
                .any(|span| span.y == 10 && span.text == "EQUIPE CONFIGURADA")
        );
        for (width, height) in [(64, 20), (96, 24), (120, 40)] {
            chat.settings_open = true;
            let frame = chat.frame(width, height);
            assert!(
                frame
                    .spans
                    .iter()
                    .any(|span| span.text.contains("32 papéis · sob demanda"))
            );
            chat.settings_open = false;
            assert!(
                frame
                    .spans
                    .iter()
                    .all(|span| span.x + span.text.width() <= usize::from(width))
            );
        }
        for _ in 0..100 {
            chat.key(key(KeyCode::PageDown), &mut db, &mut None)
                .unwrap();
        }
        assert!(
            chat.frame(64, 20)
                .spans
                .iter()
                .any(|span| span.text.contains("Integração final configurada"))
        );
        assert_eq!(chat.turns[0].prompt, "Pedido anterior preservado");
        assert_eq!(std::fs::read(&chat.options.config).unwrap(), config_before);

        compiled_profile(dir.path(), "other", 1, "sequential");
        chat.refresh_profiles();
        chat.search.value = "other".into();
        chat.pick = 0;
        chat.composer.value = "Pedido ainda não enviado".into();
        chat.choose();
        assert!(chat.note.is_empty());
        assert_eq!(chat.composer.value, "Pedido ainda não enviado");
        chat.settings_open = true;
        assert!(
            chat.frame(64, 20)
                .spans
                .iter()
                .any(|span| span.text.contains("1 papel · sequencial"))
        );
        chat.settings_open = false;
        assert!(chat.running.is_none());
        assert!(db.executions().unwrap().is_empty());
    }

    #[test]
    fn options_validation_preserves_active_values_and_prompt_paste_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.choose();
        chat.paste("original\n\tconteúdo");
        chat.paste(&"x".repeat(65536));
        assert_eq!(chat.composer.value, "original\n\tconteúdo");
        assert!(chat.notice.contains("64 KiB"));
        chat.edit_form(Action::Run).unwrap();
        let Stage::Form(editor) = &mut chat.stage else {
            panic!()
        };
        editor.inputs[2].value = "0".into();
        assert!(chat.save_form(&mut db).is_err());
        assert_eq!(chat.run_form.fields[4].value, "1800");
        let Stage::Form(editor) = &mut chat.stage else {
            panic!()
        };
        editor.inputs[2].value = "120".into();
        chat.save_form(&mut db).unwrap();
        assert_eq!(chat.run_form.fields[4].value, "120");
        assert!(chat.running.is_none());
    }

    #[test]
    fn missing_setup_cannot_enter_prompt_and_profile_filter_is_only_a_search() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.paste("not-matching");
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        assert!(chat.chosen.is_none());
        assert!(matches!(chat.stage, Stage::Profiles));
        std::fs::remove_file(&chat.options.config).unwrap();
        chat.refresh_profiles();
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        assert!(chat.chosen.is_none());
        assert!(chat.catalog.notice.contains("Setup necessário"));
    }

    #[test]
    fn frames_keep_composer_caret_visible_during_multiline_resize() {
        use unicode_width::UnicodeWidthStr;
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, _) = fixture(dir.path());
        chat.choose();
        chat.paste(&"ação\t界\n".repeat(15));
        for (w, h) in [(64, 20), (80, 24), (120, 40), (30, 10)] {
            let frame = chat.frame(w, h);
            if let Some((x, y)) = frame.caret {
                assert!(x < usize::from(w) && y < usize::from(h));
            }
            for span in frame.spans {
                assert!(span.x + span.text.width() <= usize::from(w));
                assert!(span.y < usize::from(h));
            }
        }
    }

    #[test]
    fn observed_agents_stay_beside_transcript_and_above_full_width_composer() {
        use crate::activity::AgentActivity;
        use unicode_width::UnicodeWidthStr;
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.choose();
        chat.activity.agents = (0..20)
            .map(|i| AgentActivity {
                id: format!("child-{i}"),
                parent_id: Some("root".into()),
                title: format!("Tarefa {i} — revisar arquivos"),
                model: Some("modelo-observado".into()),
                effort: Some("high".into()),
                preview: format!("Lendo src/arquivo_{i}.rs"),
                status: AgentStatus::Running,
                started_at: None,
                finished_at: None,
            })
            .collect();
        chat.paste(&"Pedido longo com Unicode 界 e espaços ".repeat(8));
        let frame = chat.frame(160, 40);
        let panel_x = 160 - chat_agents::width(160).unwrap() - 2;
        let header = frame
            .spans
            .iter()
            .find(|s| s.text.contains("SUBAGENTES"))
            .unwrap();
        assert_eq!(header.x, panel_x);
        assert!(
            frame
                .spans
                .iter()
                .any(|s| s.x >= panel_x && s.text.contains("modelo-observado"))
        );
        assert!(
            frame
                .spans
                .iter()
                .any(|s| s.x >= panel_x && s.text.contains("Lendo src/arquivo_0.rs"))
        );
        let input_top = frame
            .spans
            .iter()
            .find(|s| s.x == 2 && s.text.starts_with("───"))
            .unwrap()
            .y;
        for span in &frame.spans {
            if span.x < panel_x && span.y >= 9 && span.y < input_top {
                if span.x == panel_x - 2 && span.text == "│" {
                    continue;
                }
                assert!(span.x + span.text.width() <= panel_x - 2);
            }
        }
        assert!(
            frame
                .spans
                .iter()
                .any(|s| s.x == 2 && s.y >= input_top && s.text.width() > panel_x)
        );

        for _ in 0..20 {
            chat.key(
                KeyEvent::new(KeyCode::Down, KeyModifiers::ALT),
                &mut db,
                &mut None,
            )
            .unwrap();
        }
        let frame = chat.frame(160, 40);
        assert!(frame.spans.iter().any(|s| s.text.contains("Tarefa 19")));
        let narrow = chat.frame(64, 20);
        assert!(
            narrow
                .spans
                .iter()
                .any(|s| s.y == 7 && s.text.contains("STACKPULSE"))
        );
        assert!(
            narrow
                .spans
                .iter()
                .any(|s| s.y == 8 && s.text.contains("Sessões"))
        );
        assert!(
            narrow
                .spans
                .iter()
                .any(|s| s.y == 9 && s.text.contains("Subagentes 20"))
        );
        assert!(!narrow.spans.iter().any(|s| s.text.contains("SUBAGENTES")));
        assert!(
            narrow
                .spans
                .iter()
                .any(|s| s.text.contains("Subagentes 20"))
        );
        chat.key(key(KeyCode::F(4)), &mut db, &mut None).unwrap();
        let expanded = chat.frame(64, 20);
        assert!(expanded.spans.iter().any(|s| s.text.contains("SUBAGENTES")));
        assert!(
            expanded
                .spans
                .iter()
                .any(|s| s.text.contains("modelo-observado"))
        );
        assert!(
            expanded
                .spans
                .iter()
                .any(|s| s.text.contains("Lendo src/arquivo"))
        );
        for span in expanded.spans {
            assert!(span.x + span.text.width() <= 64);
            assert!(span.y < 20);
        }
        chat.key(key(KeyCode::Esc), &mut db, &mut None).unwrap();
        assert!(!chat.agents_expanded);
        assert_eq!(chat.activity.agents.len(), 20);
        chat.composer = Composer::default();
        chat.paste("/clear");
        chat.key(key(KeyCode::Enter), &mut db, &mut None).unwrap();
        assert!(chat.activity.agents.is_empty());
        assert!(db.executions().unwrap().is_empty());
    }

    #[test]
    fn jsonl_transfer_buttons_import_export_without_running_and_keep_draft() {
        let dir = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(dir.path());
        chat.choose();
        chat.composer.value = "Meu rascunho".into();
        chat.show_sessions().unwrap();
        chat.frame(100, 30);
        let button = chat
            .clicks
            .iter()
            .find(|(_, action)| matches!(action, ClickAction::ImportSession))
            .unwrap()
            .0;
        click(&mut chat, &mut db, button.x, button.y);
        chat.frame(100, 30);
        let source =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/history/claude.jsonl");
        chat.paste(source.to_str().unwrap());
        chat.key(key(KeyCode::F(5)), &mut db, &mut None).unwrap();
        let id = match &chat.stage {
            Stage::Sessions(picker) => picker.sessions[picker.selected].id.clone(),
            _ => panic!("import should return to the session browser"),
        };
        assert!(chat.notice.contains("1 pedido importado"));
        assert!(!chat.any_running());
        assert!(db.executions().unwrap().is_empty());
        assert_eq!(chat.composer.value, "Meu rascunho");
        chat.frame(64, 20);
        let button = chat
            .clicks
            .iter()
            .find(|(_, action)| matches!(action, ClickAction::ExportSession))
            .unwrap()
            .0;
        assert!(button.x + button.width <= 64);
        click(&mut chat, &mut db, button.x, button.y);
        let destination = dir.path().join("conversa exportada.jsonl");
        if let Stage::Form(editor) = &mut chat.stage {
            editor.inputs[0].value = destination.to_string_lossy().into_owned();
        }
        chat.key(key(KeyCode::F(5)), &mut db, &mut None).unwrap();
        assert!(chat.notice.contains("exportada"));
        let record: serde_json::Value =
            serde_json::from_str(std::fs::read_to_string(&destination).unwrap().trim()).unwrap();
        assert_eq!(record["format"], "stackpulse.session");
        chat.load_session(&id).unwrap();
        assert_eq!(chat.turns[0].prompt, "Revise a documentação da API.");
        chat.edit_transfer(true);
        chat.paste(source.to_str().unwrap());
        chat.key(key(KeyCode::F(5)), &mut db, &mut None).unwrap();
        assert!(chat.notice.contains("já foi importada"));
    }
    #[test]
    fn portable_jsonl_restores_execution_metrics_and_feedback_without_old_database() {
        let original = tempfile::tempdir().unwrap();
        let (mut chat, mut db) = fixture(original.path());
        compiled_profile(original.path(), "selected", 0, "on_demand");
        chat.refresh_profiles();
        chat.choose();
        let id = completed_measured_turn(&mut chat, &mut db, "Entrega portátil", 321);
        let mut job = chat.turns[0].execution.clone().unwrap();
        job.feedback = Some(Feedback {
            delivered: 0.8,
            speed: 4,
            note: "Feedback portátil".into(),
            recorded_at: Utc::now(),
        });
        chat.history
            .update_turn(
                &chat.session.id,
                &chat.turns[0].id,
                &chat.turns[0].lines,
                "Concluído",
                Some(&job),
                true,
            )
            .unwrap();
        let export = original.path().join("portable.jsonl");
        chat.history
            .export_session(&chat.session.id, &export)
            .unwrap();
        let destination = tempfile::tempdir().unwrap();
        let (mut other, mut fresh_db) = fixture(destination.path());
        let imported = other.history.import_session(&export).unwrap();
        assert!(fresh_db.executions().unwrap().is_empty());
        other
            .history
            .restore_execution_cache(&mut fresh_db)
            .unwrap();
        let restored = execution(&fresh_db, &id).unwrap().unwrap();
        assert_eq!(restored.reported_tokens.unwrap().input_tokens, 321);
        assert_eq!(restored.feedback.unwrap().note, "Feedback portátil");
        other.load_session(&imported.id).unwrap();
        assert_eq!(other.conversation_stats().total_tokens, Some(321));
        assert_eq!(other.conversation_stats().score, Some(8.0));
    }
}
