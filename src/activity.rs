//! Ephemeral, bounded observations of delegated work. No prompts are added to the ledger.
use crate::client::Backend;
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

pub const ENV_PATH: &str = "STACKPULSE_ACTIVITY_FILE";
const MAX_AGENTS: usize = 128;
const MAX_LINE: usize = 64 * 1024;
const MAX_SNAPSHOT: usize = 512 * 1024;
const SCAN_BYTES: usize = 512 * 1024;
const SCAN_FILES: usize = 256;
const WRITE_INTERVAL: Duration = Duration::from_millis(250);
const DISCOVER_INTERVAL: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivitySnapshot {
    pub agents: Vec<AgentActivity>,
    pub supported: bool,
    pub omitted: usize,
}
impl Default for ActivitySnapshot {
    fn default() -> Self {
        Self {
            agents: Vec::new(),
            supported: true,
            omitted: 0,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentActivity {
    pub id: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub preview: String,
    pub status: AgentStatus,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
}
impl AgentActivity {
    /// Total wall time since this agent was first observed, including pauses if
    /// it resumes. A saved snapshot without clock information stays unknown.
    pub fn elapsed_at(&self, now: DateTime<Utc>) -> Option<Duration> {
        let start = self.started_at?;
        let end = match self.finished_at {
            Some(end) => end,
            None if matches!(
                self.status,
                AgentStatus::Starting | AgentStatus::Running | AgentStatus::Waiting
            ) =>
            {
                now
            }
            None => return None,
        };
        Some((end - start).to_std().unwrap_or(Duration::ZERO))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Starting,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Unknown,
}
impl AgentStatus {
    fn active(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Running | Self::Waiting | Self::Unknown
        )
    }
}

/// Without ENV_PATH this does not inspect directories, sessions or output files.
pub struct Reporter {
    inner: Option<Inner>,
}
impl Reporter {
    pub fn from_env(client: Backend, sessions: &Path) -> Self {
        let Some(path) = std::env::var_os(ENV_PATH).filter(|p| !p.is_empty()) else {
            return Self { inner: None };
        };
        Self {
            inner: Some(Inner::new(PathBuf::from(path), client, sessions.to_owned())),
        }
    }
    pub fn update(&mut self, line: Option<&str>) {
        if let Some(inner) = &mut self.inner {
            if let Some(line) = line.filter(|s| s.len() <= MAX_LINE)
                && let Ok(value) = serde_json::from_str::<Value>(line)
            {
                inner.stream(&value);
            }
            inner.poll();
            inner.flush(false);
        }
    }
    pub fn finish(&mut self, status: AgentStatus) {
        if let Some(inner) = &mut self.inner {
            inner.last_discovery = None;
            inner.last_poll = None;
            inner.poll();
            let finished_at = Utc::now();
            for agent in &mut inner.snapshot.agents {
                if agent.status.active() {
                    // The parent's successful exit is not proof of a child's completion.
                    agent.status = match status {
                        AgentStatus::Completed => AgentStatus::Unknown,
                        AgentStatus::Failed | AgentStatus::Cancelled => status,
                        _ => AgentStatus::Unknown,
                    };
                }
                // Even an unconfirmed child stops accumulating when its owned
                // execution ends. This does not manufacture child success.
                agent.finished_at.get_or_insert(finished_at);
            }
            inner.dirty = true;
            inner.flush(true);
        }
    }
}

struct Assignment {
    parent: String,
    task: String,
    title: String,
}

struct Inner {
    output: PathBuf,
    sessions: PathBuf,
    client: Backend,
    started: DateTime<Utc>,
    root: Option<String>,
    snapshot: ActivitySnapshot,
    tails: Vec<Tail>,
    candidates: Vec<(PathBuf, Meta)>,
    inspected: HashSet<PathBuf>,
    pending: HashMap<String, String>,
    assignments: Vec<Assignment>,
    task_ids: HashMap<String, String>,
    omitted_ids: HashSet<String>,
    // Live notifications and delayed rollout records can interleave. Do not let
    // an older state override a terminal state or an explicitly resumed turn.
    status_updated_at: HashMap<String, DateTime<Utc>>,
    dirty: bool,
    last_write: Option<Instant>,
    last_discovery: Option<Instant>,
    last_poll: Option<Instant>,
}
impl Inner {
    fn new(output: PathBuf, client: Backend, sessions: PathBuf) -> Self {
        Self {
            output,
            sessions,
            client,
            started: Utc::now(),
            root: None,
            snapshot: ActivitySnapshot {
                supported: matches!(client, Backend::Codex | Backend::Claude),
                ..Default::default()
            },
            tails: Vec::new(),
            candidates: Vec::new(),
            inspected: HashSet::new(),
            pending: HashMap::new(),
            assignments: Vec::new(),
            task_ids: HashMap::new(),
            omitted_ids: HashSet::new(),
            status_updated_at: HashMap::new(),
            dirty: true,
            last_write: None,
            last_discovery: None,
            last_poll: None,
        }
    }
    fn known(&self, id: &str) -> bool {
        self.root.as_deref() == Some(id) || self.snapshot.agents.iter().any(|a| a.id == id)
    }
    fn agent(&mut self, id: &str, parent: Option<&str>) -> Option<&mut AgentActivity> {
        if !valid_id(id) || self.root.as_deref() == Some(id) {
            return None;
        }
        if let Some(index) = self.snapshot.agents.iter().position(|a| a.id == id) {
            self.dirty = true;
            return self.snapshot.agents.get_mut(index);
        }
        if self.snapshot.agents.len() >= MAX_AGENTS {
            if self.omitted_ids.len() < 4096 && self.omitted_ids.insert(id.to_owned()) {
                self.snapshot.omitted = self.snapshot.omitted.saturating_add(1);
            }
            self.dirty = true;
            return None;
        }
        self.snapshot.agents.push(AgentActivity {
            id: id.to_owned(),
            parent_id: parent.filter(|s| valid_id(s)).map(str::to_owned),
            title: "Subagente".into(),
            model: None,
            effort: None,
            preview: String::new(),
            status: AgentStatus::Starting,
            started_at: Some(Utc::now()),
            finished_at: None,
        });
        self.dirty = true;
        self.snapshot.agents.last_mut()
    }
    fn own_start(&mut self, id: &str, at: DateTime<Utc>) {
        if let Some(agent) = self.snapshot.agents.iter_mut().find(|agent| agent.id == id) {
            agent.started_at = Some(agent.started_at.map_or(at, |prior| prior.min(at)));
            self.dirty = true;
        }
    }
    fn status(&mut self, id: &str, status: AgentStatus, at: DateTime<Utc>) {
        if self
            .status_updated_at
            .get(id)
            .is_some_and(|last| *last > at)
        {
            return;
        }
        let Some(agent) = self.snapshot.agents.iter_mut().find(|agent| agent.id == id) else {
            return;
        };
        if matches!(
            status,
            AgentStatus::Starting | AgentStatus::Running | AgentStatus::Waiting
        ) && agent.finished_at.is_some_and(|end| at <= end)
        {
            return;
        }
        self.status_updated_at.insert(id.into(), at);
        agent.status = status;
        match status {
            AgentStatus::Starting | AgentStatus::Running | AgentStatus::Waiting => {
                agent.started_at.get_or_insert(at);
                agent.finished_at = None;
            }
            AgentStatus::Completed | AgentStatus::Failed | AgentStatus::Cancelled => {
                agent.finished_at.get_or_insert(at);
            }
            AgentStatus::Unknown => {}
        }
        self.dirty = true;
    }
    fn stream(&mut self, value: &Value) {
        if self.client == Backend::Claude {
            self.claude(value);
            return;
        }
        if self.client != Backend::Codex {
            return;
        }
        if value.get("method").is_some() {
            self.app_server(value);
            return;
        }
        if value["type"] == "thread.started" {
            if let Some(id) = value["thread_id"].as_str().filter(|id| valid_id(id)) {
                if self.root.as_deref().is_some_and(|root| root != id) {
                    self.snapshot.agents.clear();
                    self.snapshot.omitted = 0;
                    self.tails.clear();
                    self.candidates.clear();
                    self.inspected.clear();
                    self.pending.clear();
                    self.assignments.clear();
                    self.task_ids.clear();
                    self.omitted_ids.clear();
                    self.status_updated_at.clear();
                    self.last_discovery = None;
                    self.last_poll = None;
                }
                self.root = Some(id.to_owned());
                self.dirty = true;
            }
            return;
        }
        if !matches!(
            value["type"].as_str(),
            Some("item.started" | "item.updated" | "item.completed")
        ) {
            return;
        }
        let item = &value["item"];
        if item["type"] != "collab_tool_call" {
            return;
        }
        let Some(sender) = item["sender_thread_id"]
            .as_str()
            .filter(|id| self.known(id))
            .map(str::to_owned)
        else {
            return;
        };
        let tool = item["tool"].as_str().unwrap_or_default();
        let call_id = item["id"].as_str().filter(|id| valid_id(id));
        let prompt = item["prompt"].as_str().and_then(public_title);
        if tool == "spawn_agent"
            && let (Some(id), Some(prompt)) = (call_id, &prompt)
            && self.pending.len() < MAX_AGENTS
        {
            self.pending.insert(id.to_owned(), prompt.clone());
        }
        let title = prompt.or_else(|| call_id.and_then(|id| self.pending.get(id).cloned()));
        let ids = item["receiver_thread_ids"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let at = event_time(value);
        for id in ids.iter().take(MAX_AGENTS + 1).filter_map(Value::as_str) {
            let observed = &item["agents_states"][id];
            if let Some(agent) = self.agent(id, Some(&sender)) {
                if matches!(tool, "spawn_agent" | "send_input")
                    && let Some(title) = &title
                {
                    agent.title = title.clone();
                }
                if let Some(message) = observed["message"].as_str() {
                    agent.preview = sanitize(message, 240);
                }
            }
            if let Some(status) = observed["status"].as_str().and_then(codex_status) {
                self.status(id, status, at);
            }
        }
        if value["type"] == "item.completed"
            && let Some(id) = call_id
        {
            self.pending.remove(id);
        }
    }
    /// Only public app-server items are rendered. Reasoning and raw items are ignored.
    fn app_server(&mut self, value: &Value) {
        let method = value["method"].as_str().unwrap_or_default();
        let params = &value["params"];
        let at = event_time(value);
        if method == "thread/started" {
            let thread = &params["thread"];
            let Some(parent) = thread["parentThreadId"]
                .as_str()
                .filter(|id| self.known(id))
                .map(str::to_owned)
            else {
                return;
            };
            let Some(id) = thread["id"].as_str() else {
                return;
            };
            // Thread metadata is configuration, not observed per-turn telemetry;
            // model/effort continue to come from the child's own rollout events.
            if let Some(agent) = self.agent(id, Some(&parent))
                && let Some(title) = thread["name"]
                    .as_str()
                    .or_else(|| thread["preview"].as_str())
                    .and_then(public_title)
            {
                agent.title = title;
            }
            return;
        }
        let item = &params["item"];
        if matches!(method, "item/started" | "item/completed")
            && item["type"] == "collabAgentToolCall"
        {
            let tool = match item["tool"].as_str().unwrap_or_default() {
                "spawnAgent" => "spawn_agent",
                "sendInput" => "send_input",
                "sendMessage" => "send_message",
                "followupTask" => "followup_task",
                "interruptAgent" => "interrupt_agent",
                "closeAgent" => "close_agent",
                other => other,
            };
            self.stream(&serde_json::json!({"type":if method == "item/completed" { "item.completed" } else { "item.started" },
                "item":{"type":"collab_tool_call","id":item["id"],"tool":tool,"sender_thread_id":item["senderThreadId"],
                "receiver_thread_ids":item["receiverThreadIds"],"prompt":item["prompt"],"agents_states":item["agentsStates"]}}));
        }
        if matches!(method, "item/started" | "item/completed")
            && item["type"] == "subAgentActivity"
            && let Some(parent) = params["threadId"]
                .as_str()
                .filter(|id| self.known(id))
                .map(str::to_owned)
            && let Some(id) = item["agentThreadId"].as_str()
            && let Some(agent) = self.agent(id, Some(&parent))
        {
            if agent.title == "Subagente"
                && let Some(path) = item["agentPath"].as_str()
            {
                agent.title = task_name_title(path);
            }
            if let Some(status) = activity_status(item["kind"].as_str()) {
                self.status(id, status, at);
            }
        }
        let Some(id) = params["threadId"]
            .as_str()
            .filter(|id| self.snapshot.agents.iter().any(|a| a.id == *id))
            .map(str::to_owned)
        else {
            return;
        };
        let Some(agent) = self.agent(&id, None) else {
            return;
        };
        let mut status = None;
        match method {
            "turn/started" => status = Some(AgentStatus::Running),
            "turn/completed" => {
                status = Some(match params["turn"]["status"].as_str() {
                    Some("completed") => AgentStatus::Completed,
                    Some("failed") => AgentStatus::Failed,
                    Some("interrupted") => AgentStatus::Cancelled,
                    _ => AgentStatus::Unknown,
                });
            }
            "item/started" | "item/completed" => match item["type"].as_str() {
                Some("agentMessage") => {
                    if let Some(text) = item["text"].as_str() {
                        agent.preview = sanitize(text, 240);
                    }
                }
                Some("commandExecution") => {
                    if let Some(command) = item["command"].as_str() {
                        agent.preview = sanitize(command, 240);
                    }
                }
                _ => {}
            },
            _ => {}
        }
        if let Some(status) = status {
            self.status(&id, status, at);
        }
    }
    fn poll(&mut self) {
        if self
            .last_poll
            .is_some_and(|last| last.elapsed() < Duration::from_millis(200))
        {
            return;
        }
        self.last_poll = Some(Instant::now());
        if self.client != Backend::Codex || self.root.is_none() {
            return;
        }
        if self
            .last_discovery
            .is_none_or(|last| last.elapsed() >= DISCOVER_INTERVAL)
        {
            self.last_discovery = Some(Instant::now());
            self.discover();
        }
        let mut budget = SCAN_BYTES;
        // Round-robin so a large copied history cannot starve another agent.
        let count = self.tails.len();
        for _ in 0..count {
            if budget == 0 {
                break;
            }
            let mut tail = self.tails.remove(0);
            let (lines, read) = tail.read(MAX_LINE.min(budget));
            budget = budget.saturating_sub(read);
            for line in lines {
                if let Ok(value) = serde_json::from_slice::<Value>(&line) {
                    let ordinal = value["ordinal"].as_u64();
                    if ordinal.is_some_and(|n| tail.last_ordinal.is_some_and(|last| n <= last)) {
                        continue;
                    }
                    if let Some(ordinal) = ordinal {
                        tail.last_ordinal = Some(ordinal);
                    }
                    self.rollout(&tail.meta, &value);
                }
            }
            self.tails.push(tail);
        }
    }
    fn discover(&mut self) {
        // Standard Codex layout is YYYY/MM/DD. Do not walk historical archives.
        let mut dirs = vec![self.sessions.clone()];
        for date in [
            self.started.date_naive(),
            Utc::now().date_naive(),
            self.started.with_timezone(&Local).date_naive(),
            Local::now().date_naive(),
        ] {
            let path = self.sessions.join(date.format("%Y/%m/%d").to_string());
            if !dirs.contains(&path) {
                dirs.push(path);
            }
        }
        let cutoff: SystemTime = (self.started - chrono::Duration::seconds(2)).into();
        let mut visited = 0;
        let mut header_budget = SCAN_BYTES;
        for dir in dirs {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                visited += 1;
                if visited > 4096 || self.inspected.len() >= 4096 {
                    break;
                }
                let path = entry.path();
                if self.inspected.contains(&path) || path.extension().is_none_or(|e| e != "jsonl") {
                    continue;
                }
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if !entry.file_type().is_ok_and(|kind| kind.is_file())
                    || metadata.modified().is_ok_and(|at| at < cutoff)
                {
                    continue;
                }
                if header_budget < MAX_LINE {
                    break;
                }
                header_budget -= MAX_LINE;
                if let Some(meta) = Meta::read(&path) {
                    self.inspected.insert(path.clone());
                    if meta.created >= self.started - chrono::Duration::seconds(2)
                        && self.candidates.len() < SCAN_FILES
                    {
                        self.candidates.push((path, meta));
                    }
                }
            }
        }
        // A grandchild may precede its parent in directory order.
        for _ in 0..MAX_AGENTS {
            let Some(index) = self.candidates.iter().position(|(_, m)| {
                self.root.as_deref() == Some(&m.id)
                    || m.parent.as_deref().is_some_and(|p| self.known(p))
            }) else {
                break;
            };
            let (path, meta) = self.candidates.remove(index);
            if self.tails.iter().any(|tail| tail.meta.id == meta.id) {
                continue;
            }
            if self.tails.len() > MAX_AGENTS {
                break;
            }
            if self.root.as_deref() != Some(&meta.id) {
                if let Some(agent) = self.agent(&meta.id, meta.parent.as_deref()) {
                    if agent.title == "Subagente"
                        && let Some(name) = &meta.name
                    {
                        agent.title = task_name_title(name);
                    }
                } else {
                    continue;
                }
                self.own_start(&meta.id, meta.created);
            }
            self.assign_title(&meta.id, meta.parent.as_deref(), meta.name.as_deref());
            self.tails.push(Tail::new(path, meta));
        }
    }
    fn rollout(&mut self, meta: &Meta, value: &Value) {
        // Forked rollouts contain copied parent messages. Never expose them or
        // inherit their model as an observation about this child's execution.
        let Some(at) = value["timestamp"].as_str().and_then(parse_time) else {
            return;
        };
        if meta.history_start.is_some_and(|start| {
            value["ordinal"]
                .as_u64()
                .is_none_or(|ordinal| ordinal < start)
        }) || at < meta.created
        {
            return;
        }
        let payload = &value["payload"];
        self.delegation(meta, value);
        if self.root.as_deref() == Some(&meta.id) {
            return;
        }
        self.own_start(&meta.id, meta.created);
        let Some(agent) = self.agent(&meta.id, meta.parent.as_deref()) else {
            return;
        };
        let mut status = None;
        match value["type"].as_str().unwrap_or_default() {
            "turn_context" => {
                if let Some(model) = payload["model"].as_str() {
                    agent.model = nonempty(model, 120);
                }
                if let Some(effort) = payload["effort"]
                    .as_str()
                    .or_else(|| payload["reasoning_effort"].as_str())
                {
                    agent.effort = nonempty(effort, 40);
                }
            }
            "event_msg" => match payload["type"].as_str().unwrap_or_default() {
                "task_started" => status = Some(AgentStatus::Running),
                "task_complete" => status = Some(AgentStatus::Completed),
                "turn_aborted" => status = Some(AgentStatus::Cancelled),
                "item_completed" => {
                    let item = &payload["item"];
                    if item["type"] == "CommandExecution" {
                        if let Some(command) = item["command"].as_array() {
                            agent.preview = sanitize(
                                &command
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join(" "),
                                240,
                            );
                        }
                    } else if item["type"] == "AgentMessage"
                        && let Some(preview) = public_message(item)
                    {
                        agent.preview = preview;
                    }
                }
                "agent_message" => {
                    if let Some(text) = payload["message"].as_str() {
                        agent.preview = sanitize(text, 240);
                    }
                }
                "exec_command_begin" => {
                    if let Some(command) = payload["command"].as_array() {
                        agent.preview = sanitize(
                            &command
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(" "),
                            240,
                        );
                        if agent.status.active() && agent.finished_at.is_none() {
                            status = Some(AgentStatus::Running);
                        }
                    }
                }
                _ => {}
            },
            "response_item" => {
                if let Some(preview) = public_preview(payload) {
                    agent.preview = preview;
                }
            }
            _ => {}
        }
        if let Some(status) = status {
            self.status(&meta.id, status, at);
        }
    }
    fn assign_title(&mut self, id: &str, parent: Option<&str>, name: Option<&str>) {
        let title = self
            .assignments
            .iter()
            .rev()
            .find(|a| {
                parent == Some(a.parent.as_str())
                    && name.is_some_and(|name| {
                        name == a.task || name.ends_with(&format!("/{}", a.task))
                    })
            })
            .map(|a| a.title.clone());
        if let Some(title) = title
            && let Some(agent) = self.agent(id, parent)
        {
            agent.title = title;
        }
    }
    fn delegation(&mut self, meta: &Meta, value: &Value) {
        let payload = &value["payload"];
        if value["type"] == "response_item"
            && payload["type"] == "function_call"
            && payload["name"] == "spawn_agent"
            && payload["namespace"] == "collaboration"
            && let Some(arguments) = payload["arguments"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
            && let (Some(task), Some(message)) = (
                arguments["task_name"].as_str(),
                arguments["message"].as_str(),
            )
            && self.assignments.len() < MAX_AGENTS
        {
            self.assignments.push(Assignment {
                parent: meta.id.clone(),
                task: sanitize(task, 120),
                title: delegation_title(task, message),
            });
            // Child metadata may have arrived before the parent's spawn record.
            let children: Vec<_> = self
                .tails
                .iter()
                .filter(|tail| tail.meta.parent.as_deref() == Some(&meta.id))
                .map(|tail| tail.meta.clone())
                .collect();
            for child in children {
                self.assign_title(&child.id, child.parent.as_deref(), child.name.as_deref());
            }
        }
        if value["type"] != "event_msg" || payload["type"] != "item_completed" {
            return;
        }
        let item = &payload["item"];
        if item["type"] == "CollabAgentToolCall" {
            let mut event = value.clone();
            event["type"] = "item.completed".into();
            event["item"] = item.clone();
            event["item"]["type"] = "collab_tool_call".into();
            self.stream(&event);
        }
        if item["type"] != "SubAgentActivity" {
            return;
        }
        let Some(id) = item["agent_thread_id"].as_str() else {
            return;
        };
        let name = item["agent_path"].as_str();
        if let Some(agent) = self.agent(id, Some(&meta.id))
            && agent.title == "Subagente"
            && let Some(name) = name
        {
            agent.title = task_name_title(name);
        }
        if let Some(status) = activity_status(item["kind"].as_str()) {
            self.status(id, status, event_time(value));
        }
        self.assign_title(id, Some(&meta.id), name);
    }
    fn claude(&mut self, value: &Value) {
        let parent = value["parent_tool_use_id"].as_str();
        let at = event_time(value);
        if value["type"] == "assistant" {
            let message = &value["message"];
            if let Some(id) = parent.filter(|id| self.known(id))
                && let Some(agent) = self.agent(id, None)
            {
                if let Some(model) = message["model"].as_str() {
                    agent.model = nonempty(model, 120);
                }
                let running = agent.status.active() && agent.finished_at.is_none();
                if let Some(blocks) = message["content"].as_array() {
                    for block in blocks {
                        if block["type"] == "text"
                            && let Some(text) = block["text"].as_str()
                        {
                            agent.preview = sanitize(text, 240);
                        } else if block["type"] == "tool_use"
                            && let Some(name) = block["name"].as_str()
                        {
                            agent.preview = tool_preview(name, &block["input"]);
                        }
                    }
                }
                // A late final message does not itself restart a completed task.
                if running {
                    self.status(id, AgentStatus::Running, at);
                }
            }
            if let Some(blocks) = message["content"].as_array() {
                for block in blocks {
                    if block["type"] != "tool_use"
                        || !matches!(block["name"].as_str(), Some("Agent" | "Task"))
                    {
                        continue;
                    }
                    if parent.is_some_and(|id| !self.known(id)) {
                        continue;
                    }
                    if let Some(id) = block["id"].as_str()
                        && let Some(agent) = self.agent(id, parent)
                    {
                        if let Some(title) = block["input"]["description"]
                            .as_str()
                            .or_else(|| block["input"]["prompt"].as_str())
                        {
                            agent.title = sanitize(title, 120);
                        }
                        // input.model is a requested alias; only child message.model is observed.
                        if agent.finished_at.is_none() {
                            self.status(id, AgentStatus::Starting, at);
                        }
                    }
                }
            }
        }
        if value["type"] != "system" {
            return;
        }
        let subtype = value["subtype"].as_str().unwrap_or_default();
        let task = value["task_id"].as_str().filter(|id| valid_id(id));
        let tool = value["tool_use_id"].as_str().filter(|id| valid_id(id));
        if subtype == "task_started" && value["task_type"] == "local_agent" {
            let Some(id) = tool.or(task) else {
                return;
            };
            if let Some(task) = task
                && self.task_ids.len() < MAX_AGENTS
            {
                self.task_ids.insert(task.to_owned(), id.to_owned());
            }
            if let Some(agent) = self.agent(id, parent) {
                if let Some(description) = value["description"].as_str() {
                    agent.title = sanitize(description, 120);
                }
                self.status(id, AgentStatus::Running, at);
            }
        }
        let id = tool
            .filter(|id| self.known(id))
            .map(str::to_owned)
            .or_else(|| task.and_then(|task| self.task_ids.get(task).cloned()));
        let Some(id) = id else {
            return;
        };
        let Some(agent) = self.agent(&id, parent) else {
            return;
        };
        let mut next_status = None;
        match subtype {
            "task_progress" => {
                if let Some(summary) = value["summary"]
                    .as_str()
                    .or_else(|| value["last_tool_name"].as_str())
                {
                    agent.preview = sanitize(summary, 240);
                }
            }
            "task_notification" => {
                if let Some(status) = value["status"].as_str().and_then(claude_status) {
                    next_status = Some(status);
                }
                if let Some(summary) = value["summary"].as_str() {
                    agent.preview = sanitize(summary, 240);
                }
            }
            "task_updated" => {
                if let Some(status) = value["patch"]["status"].as_str().and_then(claude_status) {
                    next_status = Some(status);
                }
            }
            _ => {}
        }
        if let Some(status) = next_status {
            self.status(&id, status, at);
        }
    }
    fn flush(&mut self, force: bool) {
        if !self.dirty
            || (!force
                && self
                    .last_write
                    .is_some_and(|last| last.elapsed() < WRITE_INTERVAL))
        {
            return;
        }
        self.last_write = Some(Instant::now());
        let Ok(bytes) = serde_json::to_vec(&self.snapshot) else {
            return;
        };
        if bytes.len() > MAX_SNAPSHOT {
            return;
        }
        let Some(parent) = self.output.parent() else {
            return;
        };
        let result = (|| -> std::io::Result<()> {
            // NamedTempFile is mode 0600 on Unix; rename publishes one complete snapshot.
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            temp.write_all(&bytes)?;
            temp.flush()?;
            temp.persist(&self.output).map_err(|e| e.error)?;
            Ok(())
        })();
        if result.is_ok() {
            self.dirty = false;
        }
    }
}

#[derive(Clone)]
struct Meta {
    id: String,
    parent: Option<String>,
    name: Option<String>,
    created: DateTime<Utc>,
    history_start: Option<u64>,
}
impl Meta {
    fn read(path: &Path) -> Option<Self> {
        let mut bytes = Vec::new();
        File::open(path)
            .ok()?
            .take(MAX_LINE as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        let end = bytes.iter().position(|&b| b == b'\n')?;
        let value: Value = serde_json::from_slice(&bytes[..end]).ok()?;
        if value["type"] != "session_meta" {
            return None;
        }
        let payload = &value["payload"];
        let spawn = &payload["source"]["subagent"]["thread_spawn"];
        Some(Self {
            id: payload["id"].as_str().filter(|id| valid_id(id))?.to_owned(),
            parent: spawn["parent_thread_id"]
                .as_str()
                .or_else(|| payload["parent_thread_id"].as_str())
                .filter(|id| valid_id(id))
                .map(str::to_owned),
            name: spawn["agent_path"]
                .as_str()
                .or_else(|| spawn["agent_nickname"].as_str())
                .and_then(|s| nonempty(s, 120)),
            created: payload["timestamp"].as_str().and_then(parse_time)?,
            history_start: payload["subagent_history_start_ordinal"].as_u64(),
        })
    }
}
struct Tail {
    path: PathBuf,
    meta: Meta,
    offset: u64,
    partial: Vec<u8>,
    discarding: bool,
    last_ordinal: Option<u64>,
    identity: Option<(u64, u64)>,
    initialized: bool,
}
impl Tail {
    fn new(path: PathBuf, meta: Meta) -> Self {
        Self {
            path,
            meta,
            offset: 0,
            partial: Vec::new(),
            discarding: false,
            last_ordinal: None,
            identity: None,
            initialized: false,
        }
    }
    fn read(&mut self, limit: usize) -> (Vec<Vec<u8>>, usize) {
        let mut lines = Vec::new();
        let Ok(mut file) = File::open(&self.path) else {
            return (lines, 0);
        };
        let Ok(metadata) = file.metadata() else {
            return (lines, 0);
        };
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            Some((metadata.dev(), metadata.ino()))
        };
        #[cfg(not(unix))]
        let identity = None;
        if (self.identity.is_some() && self.identity != identity || metadata.len() < self.offset)
            && Meta::read(&self.path).is_none_or(|meta| {
                meta.id != self.meta.id
                    || meta.parent != self.meta.parent
                    || meta.created != self.meta.created
            })
        {
            return (lines, 0);
        }
        let first_read = !self.initialized;
        self.initialized = true;
        self.identity = identity;
        if first_read && self.meta.parent.is_some() && metadata.len() > SCAN_BYTES as u64 {
            // A fork can copy megabytes of parent history. Start in a bounded
            // live window and discard its first partial line. Missing older
            // model metadata stays unknown until actually observed again.
            self.offset = metadata.len() - SCAN_BYTES as u64;
            self.discarding = true;
        }
        if metadata.len() < self.offset {
            self.offset = 0;
            self.partial.clear();
            self.discarding = false;
            // Replacement content must still identify the same observed session.
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return (lines, 0);
        }
        let mut chunk = vec![0; limit];
        let Ok(read) = file.read(&mut chunk) else {
            return (lines, 0);
        };
        self.offset += read as u64;
        for &byte in &chunk[..read] {
            if byte == b'\n' {
                if !self.discarding && !self.partial.is_empty() {
                    lines.push(std::mem::take(&mut self.partial));
                }
                self.partial.clear();
                self.discarding = false;
            } else if !self.discarding {
                if self.partial.len() < MAX_LINE {
                    self.partial.push(byte);
                } else {
                    self.partial.clear();
                    self.discarding = true;
                }
            }
        }
        (lines, read)
    }
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.'))
}
fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}
fn event_time(value: &Value) -> DateTime<Utc> {
    value["timestamp"]
        .as_str()
        .and_then(parse_time)
        .unwrap_or_else(Utc::now)
}
fn activity_status(kind: Option<&str>) -> Option<AgentStatus> {
    match kind {
        Some("started") => Some(AgentStatus::Running),
        Some("interrupted") => Some(AgentStatus::Cancelled),
        Some("completed") => Some(AgentStatus::Completed),
        // V2 send_message emits "interacted" even when it only queues input
        // for an idle agent. Only an actual start reopens the timer.
        _ => None,
    }
}
fn codex_status(value: &str) -> Option<AgentStatus> {
    Some(match value {
        "pending_init" | "pendingInit" => AgentStatus::Starting,
        "running" => AgentStatus::Running,
        "interrupted" => AgentStatus::Cancelled,
        "completed" => AgentStatus::Completed,
        "errored" | "not_found" | "notFound" => AgentStatus::Failed,
        "shutdown" => AgentStatus::Cancelled,
        _ => return None,
    })
}
fn nonempty(value: &str, max: usize) -> Option<String> {
    let text = sanitize(value, max);
    (!text.is_empty()).then_some(text)
}
fn public_title(value: &str) -> Option<String> {
    let title = sanitize(value, 120);
    if title.is_empty() || opaque_token(&title) {
        None
    } else {
        Some(title)
    }
}
fn delegation_title(task: &str, message: &str) -> String {
    public_title(message).unwrap_or_else(|| task_name_title(task))
}
fn task_name_title(value: &str) -> String {
    let task = value
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(value);
    let normalized = sanitize(&task.replace(['_', '-'], " "), 120);
    if normalized.is_empty() || opaque_token(task) {
        return "Subagente".into();
    }
    let mut chars = normalized.chars();
    let Some(first) = chars.next() else {
        return "Subagente".into();
    };
    first.to_uppercase().chain(chars).collect()
}
fn opaque_token(value: &str) -> bool {
    let value = value.trim();
    (value.len() >= 64
        && !value.chars().any(char::is_whitespace)
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '=')))
        || (value.len() >= 24
            && value.chars().filter(|&c| c == '-').count() >= 3
            && value.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
}
fn public_preview(value: &Value) -> Option<String> {
    match value["type"].as_str()? {
        "message" if value["role"] == "assistant" => public_message(value),
        "function_call" | "custom_tool_call" => {
            let name = value["name"].as_str()?;
            let arguments = value["arguments"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok());
            let detail = arguments.as_ref().and_then(|args| {
                ["cmd", "command", "path", "file_path", "query", "q"]
                    .iter()
                    .find_map(|key| args[*key].as_str())
            });
            // A tool's argument is public activity, but arbitrary JSON and tool
            // reasoning are not copied into a preview.
            Some(sanitize(
                &detail
                    .map(|s| format!("{name} · {s}"))
                    .unwrap_or_else(|| name.to_owned()),
                240,
            ))
        }
        _ => None,
    }
}
/// Removes terminal escapes, controls and Unicode directional overrides.
fn sanitize(value: &str, max: usize) -> String {
    let mut result = String::new();
    let mut chars = value.chars().peekable();
    let mut count = 0;
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.peek() == Some(&'\\')) {
                            if c == '\u{1b}' {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            continue;
        }
        let c = if c.is_whitespace() {
            ' '
        } else if c.is_control() {
            continue;
        } else {
            c
        };
        if c == ' ' && (result.is_empty() || result.ends_with(' ')) {
            continue;
        }
        if count >= max {
            break;
        }
        result.push(c);
        count += 1;
    }
    result.trim().to_owned()
}

fn claude_status(value: &str) -> Option<AgentStatus> {
    Some(match value {
        "pending" => AgentStatus::Starting,
        "running" => AgentStatus::Running,
        "completed" => AgentStatus::Completed,
        "failed" => AgentStatus::Failed,
        "killed" | "stopped" => AgentStatus::Cancelled,
        "paused" => AgentStatus::Waiting,
        _ => return None,
    })
}
fn tool_preview(name: &str, input: &Value) -> String {
    let detail = ["cmd", "command", "path", "file_path", "query", "q"]
        .iter()
        .find_map(|key| input[*key].as_str());
    sanitize(
        &detail
            .map(|s| format!("{name} · {s}"))
            .unwrap_or_else(|| name.to_owned()),
        240,
    )
}
fn public_message(value: &Value) -> Option<String> {
    if value["channel"]
        .as_str()
        .is_some_and(|s| !matches!(s, "commentary" | "final"))
        || value["phase"]
            .as_str()
            .is_some_and(|s| !matches!(s, "commentary" | "final_answer"))
    {
        return None;
    }
    let text = value["content"]
        .as_array()?
        .iter()
        .filter(|part| matches!(part["type"].as_str(), Some("text" | "output_text" | "Text")))
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    nonempty(&text, 240)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> (tempfile::TempDir, Inner) {
        let dir = tempfile::tempdir().unwrap();
        let inner = Inner::new(
            dir.path().join("snapshot.json"),
            Backend::Codex,
            dir.path().join("sessions"),
        );
        fs::create_dir(&inner.sessions).unwrap();
        (dir, inner)
    }
    fn poll_now(inner: &mut Inner) {
        inner.last_poll = None;
        inner.poll();
    }
    fn root(inner: &mut Inner) {
        inner.stream(&json!({"type":"thread.started","thread_id":"root"}));
    }
    fn spawn(inner: &mut Inner, id: &str) {
        inner.stream(&json!({"type":"item.completed","item":{"type":"collab_tool_call","id":"call1","tool":"spawn_agent","sender_thread_id":"root","receiver_thread_ids":[id],"prompt":"Implementar os testes de integração","status":"completed","agents_states":{id:{"status":"running"}}}}));
    }
    fn metadata(inner: &Inner, id: &str, parent: &str, history: Option<u64>) -> Value {
        json!({"timestamp":inner.started.to_rfc3339(),"ordinal":0,"type":"session_meta","payload":{"id":id,"timestamp":inner.started.to_rfc3339(),"source":{"subagent":{"thread_spawn":{"parent_thread_id":parent,"agent_path":format!("/root/{id}")}}},"subagent_history_start_ordinal":history}})
    }
    fn record(inner: &Inner, kind: &str, ordinal: u64, payload: Value) -> Value {
        json!({"timestamp":(inner.started+chrono::Duration::milliseconds(30)).to_rfc3339(),"ordinal":ordinal,"type":kind,"payload":payload})
    }
    fn write_session(inner: &Inner, name: &str, rows: &[Value]) -> PathBuf {
        let path = inner.sessions.join(format!("{name}.jsonl"));
        let mut file = File::create(&path).unwrap();
        for row in rows {
            writeln!(file, "{row}").unwrap();
        }
        path
    }
    fn clock_at(seconds: i64) -> DateTime<Utc> {
        parse_time("2025-01-01T00:00:00Z").unwrap() + chrono::Duration::seconds(seconds)
    }
    #[test]
    fn old_snapshots_keep_unknown_duration_and_clock_never_goes_negative() {
        let mut agent: AgentActivity = serde_json::from_value(json!({"id":"old","parent_id":null,"title":"Older run","model":null,"effort":null,"preview":"","status":"completed"})).unwrap();
        assert_eq!(agent.elapsed_at(clock_at(100)), None);
        assert_eq!(agent.started_at, None);
        assert_eq!(agent.finished_at, None);
        agent.started_at = Some(clock_at(10));
        assert_eq!(agent.elapsed_at(clock_at(100)), None);
        agent.status = AgentStatus::Unknown;
        assert_eq!(agent.elapsed_at(clock_at(100)), None);
        agent.status = AgentStatus::Running;
        assert_eq!(agent.elapsed_at(clock_at(5)), Some(Duration::ZERO));
        agent.finished_at = Some(clock_at(4));
        assert_eq!(agent.elapsed_at(clock_at(100)), Some(Duration::ZERO));
        agent.finished_at = Some(clock_at(20));
        let restored: AgentActivity =
            serde_json::from_str(&serde_json::to_string(&agent).unwrap()).unwrap();
        assert_eq!(
            restored.elapsed_at(clock_at(100)),
            Some(Duration::from_secs(10))
        );
    }
    #[test]
    fn clocks_are_independent_freeze_and_resume_without_replaying_old_states() {
        let (_dir, mut inner) = fixture();
        inner.agent("first", None).unwrap();
        inner.agent("second", None).unwrap();
        inner.own_start("first", clock_at(0));
        inner.own_start("second", clock_at(5));
        inner.status("first", AgentStatus::Running, clock_at(1));
        inner.status("second", AgentStatus::Running, clock_at(6));
        inner.status("first", AgentStatus::Completed, clock_at(8));
        assert_eq!(
            inner.snapshot.agents[0].elapsed_at(clock_at(15)),
            Some(Duration::from_secs(8))
        );
        assert_eq!(
            inner.snapshot.agents[1].elapsed_at(clock_at(15)),
            Some(Duration::from_secs(10))
        );
        let meta = Meta {
            id: "first".into(),
            parent: Some("root".into()),
            name: None,
            created: clock_at(0),
            history_start: Some(1),
        };
        inner.rollout(&meta, &json!({"type":"event_msg","timestamp":clock_at(3),"ordinal":1,"payload":{"type":"task_started"}}));
        inner.status("first", AgentStatus::Running, clock_at(8));
        assert_eq!(inner.snapshot.agents[0].finished_at, Some(clock_at(8)));
        inner.rollout(&meta, &json!({"type":"event_msg","timestamp":clock_at(20),"ordinal":2,"payload":{"type":"task_started"}}));
        assert_eq!(inner.snapshot.agents[0].started_at, Some(clock_at(0)));
        assert_eq!(inner.snapshot.agents[0].finished_at, None);
        assert_eq!(
            inner.snapshot.agents[0].elapsed_at(clock_at(25)),
            Some(Duration::from_secs(25))
        );
        inner.status("first", AgentStatus::Completed, clock_at(8));
        assert_eq!(inner.snapshot.agents[0].status, AgentStatus::Running);
        inner.status("first", AgentStatus::Failed, clock_at(30));
        inner.status("first", AgentStatus::Failed, clock_at(40));
        assert_eq!(inner.snapshot.agents[0].finished_at, Some(clock_at(30)));
    }
    #[test]
    fn first_observation_is_per_agent_and_never_copied_from_root_start() {
        let (_dir, mut inner) = fixture();
        inner.started = Utc::now() - chrono::Duration::days(1);
        let before = Utc::now();
        let agent = inner.agent("child", None).unwrap();
        assert!(agent.started_at.unwrap() >= before);
        assert!(agent.started_at.unwrap() <= Utc::now());
    }
    #[test]
    fn app_server_terminal_and_explicit_restart_update_clock_but_messages_do_not() {
        let (_dir, mut inner) = fixture();
        root(&mut inner);
        inner.stream(&json!({"method":"thread/started","params":{"thread":{"id":"child","parentThreadId":"root"}}}));
        inner.own_start("child", clock_at(0));
        inner.stream(&json!({"timestamp":clock_at(2),"method":"turn/started","params":{"threadId":"child","turn":{"id":"first"}}}));
        inner.stream(&json!({"timestamp":clock_at(7),"method":"turn/completed","params":{"threadId":"child","turn":{"id":"first","status":"failed"}}}));
        assert_eq!(inner.snapshot.agents[0].finished_at, Some(clock_at(7)));
        inner.stream(&json!({"timestamp":clock_at(9),"method":"item/completed","params":{"threadId":"root","item":{"type":"subAgentActivity","agentThreadId":"child","kind":"interacted"}}}));
        assert_eq!(inner.snapshot.agents[0].finished_at, Some(clock_at(7)));
        inner.stream(&json!({"timestamp":clock_at(12),"method":"item/completed","params":{"threadId":"root","item":{"type":"subAgentActivity","agentThreadId":"child","kind":"started"}}}));
        assert_eq!(inner.snapshot.agents[0].finished_at, None);
        inner.stream(&json!({"timestamp":clock_at(14),"method":"item/completed","params":{"threadId":"root","item":{"type":"subAgentActivity","agentThreadId":"child","kind":"interrupted"}}}));
        assert_eq!(
            inner.snapshot.agents[0].elapsed_at(clock_at(100)),
            Some(Duration::from_secs(14))
        );
    }
    #[test]
    fn claude_completion_late_message_and_explicit_resume_keep_the_right_clock() {
        let (_dir, mut inner) = fixture();
        inner.client = Backend::Claude;
        inner.stream(&json!({"timestamp":clock_at(1),"type":"system","subtype":"task_started","task_type":"local_agent","task_id":"task","tool_use_id":"child"}));
        inner.own_start("child", clock_at(0));
        inner.stream(&json!({"timestamp":clock_at(8),"type":"system","subtype":"task_notification","task_id":"task","status":"completed"}));
        inner.stream(&json!({"timestamp":clock_at(9),"type":"assistant","parent_tool_use_id":"child","message":{"content":[{"type":"text","text":"Late final output"}]}}));
        assert_eq!(inner.snapshot.agents[0].finished_at, Some(clock_at(8)));
        inner.stream(&json!({"timestamp":clock_at(20),"type":"system","subtype":"task_updated","task_id":"task","patch":{"status":"running"}}));
        assert_eq!(inner.snapshot.agents[0].finished_at, None);
        inner.stream(&json!({"timestamp":clock_at(25),"type":"system","subtype":"task_notification","task_id":"task","status":"killed"}));
        assert_eq!(inner.snapshot.agents[0].status, AgentStatus::Cancelled);
        assert_eq!(
            inner.snapshot.agents[0].elapsed_at(clock_at(100)),
            Some(Duration::from_secs(25))
        );
    }
    #[test]
    fn stream_spawn_observes_title_but_never_invents_requested_model_or_completion() {
        let (_dir, mut inner) = fixture();
        root(&mut inner);
        spawn(&mut inner, "child");
        assert_eq!(inner.snapshot.agents.len(), 1);
        let agent = &inner.snapshot.agents[0];
        assert_eq!(agent.title, "Implementar os testes de integração");
        assert_eq!(agent.model, None);
        assert_eq!(agent.status, AgentStatus::Running);
        inner.stream(&json!({"type":"item.completed","item":{"type":"collab_tool_call","tool":"wait","sender_thread_id":"root","receiver_thread_ids":["child"],"agents_states":{"child":{"status":"completed","message":"Os testes passaram."}}}}));
        assert_eq!(inner.snapshot.agents[0].status, AgentStatus::Completed);
        assert_eq!(inner.snapshot.agents[0].preview, "Os testes passaram.");
    }
    #[test]
    fn app_server_cards_only_show_public_child_activity() {
        let (_dir, mut inner) = fixture();
        root(&mut inner);
        inner.stream(&json!({"method":"thread/started","params":{"thread":{"id":"child","parentThreadId":"foreign","name":"foreign"}}}));
        assert!(inner.snapshot.agents.is_empty());
        inner.stream(&json!({"method":"thread/started","params":{"thread":{"id":"child","parentThreadId":"root","name":"Validar entradas","model":"configured-only"}}}));
        inner.stream(&json!({"method":"item/started","params":{"threadId":"child","item":{"type":"commandExecution","command":"cargo test validation"}}}));
        inner.stream(&json!({"method":"rawResponseItem/completed","params":{"threadId":"child","item":{"type":"reasoning","content":"SECRET"}}}));
        assert_eq!(inner.snapshot.agents[0].title, "Validar entradas");
        assert_eq!(inner.snapshot.agents[0].preview, "cargo test validation");
        assert_eq!(inner.snapshot.agents[0].model, None);
        inner.stream(&json!({"method":"turn/completed","params":{"threadId":"child","turn":{"status":"completed"}}}));
        assert_eq!(inner.snapshot.agents[0].status, AgentStatus::Completed);
    }
    #[test]
    fn live_rollout_ignores_inherited_ordinals_even_when_timestamps_were_rewritten() {
        let (_dir, mut inner) = fixture();
        root(&mut inner);
        let path = write_session(
            &inner,
            "child",
            &[
                metadata(&inner, "child", "root", Some(10)),
                record(
                    &inner,
                    "turn_context",
                    1,
                    json!({"model":"parent-secret-model"}),
                ),
                record(
                    &inner,
                    "response_item",
                    2,
                    json!({"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"PRIVATE PARENT HISTORY"}]}),
                ),
                record(
                    &inner,
                    "turn_context",
                    10,
                    json!({"model":"gpt-5.6-luna","effort":"max"}),
                ),
                record(&inner, "event_msg", 11, json!({"type":"task_started"})),
                record(
                    &inner,
                    "response_item",
                    12,
                    json!({"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cargo test\"}"}),
                ),
            ],
        );
        poll_now(&mut inner);
        let agent = &inner.snapshot.agents[0];
        assert_eq!(agent.model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(agent.effort.as_deref(), Some("max"));
        assert_eq!(agent.preview, "exec_command · cargo test");
        assert_eq!(agent.status, AgentStatus::Running);
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(
            file,
            "{}",
            record(&inner, "event_msg", 13, json!({"type":"task_complete"}))
        )
        .unwrap();
        poll_now(&mut inner);
        assert_eq!(inner.snapshot.agents[0].status, AgentStatus::Completed);
    }
    #[test]
    fn v2_encrypted_assignment_uses_readable_task_name_and_keeps_correlation() {
        let (_dir, mut inner) = fixture();
        root(&mut inner);
        write_session(
            &inner,
            "root",
            &[
                metadata(&inner, "root", "", None),
                record(
                    &inner,
                    "response_item",
                    1,
                    json!({"type":"function_call","namespace":"collaboration","name":"spawn_agent","arguments":format!("{{\"task_name\":\"revisar_acessibilidade_terminal\",\"message\":\"gAAAAA{}\",\"model\":\"requested-not-observed\"}}", "x".repeat(100))}),
                ),
                record(
                    &inner,
                    "event_msg",
                    2,
                    json!({"type":"item_completed","item":{"type":"SubAgentActivity","kind":"started","agent_thread_id":"child","agent_path":"/root/revisar_acessibilidade_terminal"}}),
                ),
            ],
        );
        write_session(
            &inner,
            "child",
            &[
                metadata(&inner, "child", "root", Some(4)),
                record(
                    &inner,
                    "turn_context",
                    4,
                    json!({"model":"luna-observed","effort":"high"}),
                ),
            ],
        );
        poll_now(&mut inner);
        let agent = &inner.snapshot.agents[0];
        assert_eq!(agent.title, "Revisar acessibilidade terminal");
        assert_eq!(agent.status, AgentStatus::Running);
        assert_eq!(agent.model.as_deref(), Some("luna-observed"));
    }
    #[test]
    fn public_assignment_message_stays_the_title_and_technical_paths_are_humanized() {
        assert_eq!(
            delegation_title(
                "revisar_acessibilidade_terminal",
                "Revisar acessibilidade do terminal"
            ),
            "Revisar acessibilidade do terminal"
        );
        assert_eq!(
            task_name_title("/root/titulo_tarefa_subagente"),
            "Titulo tarefa subagente"
        );
        assert_eq!(
            task_name_title("01a0a266-650b-7c71-8b98-2c073ac5a4ae"),
            "Subagente"
        );
    }
    #[test]
    fn unrelated_and_old_sessions_are_excluded_but_grandchildren_are_linked() {
        let (_dir, mut inner) = fixture();
        root(&mut inner);
        write_session(
            &inner,
            "unrelated",
            &[metadata(&inner, "other", "unrelated-root", None)],
        );
        write_session(
            &inner,
            "grandchild",
            &[metadata(&inner, "grandchild", "child", None)],
        );
        write_session(&inner, "child", &[metadata(&inner, "child", "root", None)]);
        let mut old = metadata(&inner, "old", "root", None);
        old["payload"]["timestamp"] = (inner.started - chrono::Duration::days(1))
            .to_rfc3339()
            .into();
        write_session(&inner, "old", &[old]);
        poll_now(&mut inner);
        assert_eq!(inner.snapshot.agents.len(), 2);
        assert!(
            inner
                .snapshot
                .agents
                .iter()
                .all(|a| a.id == "child" || a.id == "grandchild")
        );
    }
    #[test]
    fn timestamp_fallback_and_reasoning_never_leak_into_preview() {
        let (_dir, mut inner) = fixture();
        root(&mut inner);
        let mut old = record(
            &inner,
            "response_item",
            1,
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"copied secret"}]}),
        );
        old["timestamp"] = (inner.started - chrono::Duration::seconds(1))
            .to_rfc3339()
            .into();
        write_session(
            &inner,
            "child",
            &[
                metadata(&inner, "child", "root", None),
                old,
                record(
                    &inner,
                    "response_item",
                    2,
                    json!({"type":"reasoning","summary":"SECRET"}),
                ),
                record(
                    &inner,
                    "response_item",
                    3,
                    json!({"type":"message","role":"assistant","phase":"analysis","content":[{"type":"output_text","text":"SECRET"}]}),
                ),
                record(
                    &inner,
                    "response_item",
                    4,
                    json!({"type":"agent_message","content":[{"type":"encrypted_content","text":"SECRET"}]}),
                ),
            ],
        );
        poll_now(&mut inner);
        assert_eq!(inner.snapshot.agents[0].preview, "");
        assert_eq!(inner.snapshot.agents[0].model, None);
    }
    #[test]
    fn public_v2_message_uses_pascal_case_text() {
        assert_eq!(public_message(&json!({"type":"AgentMessage","phase":"commentary","content":[{"type":"Text","text":"Executando testes"}]})).as_deref(),Some("Executando testes"));
        assert!(
            public_message(
                &json!({"phase":"analysis","content":[{"type":"Text","text":"SECRET"}]})
            )
            .is_none()
        );
    }
    #[test]
    fn partial_and_oversized_lines_are_bounded_and_recover() {
        let (_dir, mut inner) = fixture();
        root(&mut inner);
        let path = write_session(&inner, "child", &[metadata(&inner, "child", "root", None)]);
        poll_now(&mut inner);
        let good = record(&inner, "turn_context", 1, json!({"model":"actual"})).to_string();
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "{}", &good[..good.len() / 2]).unwrap();
        poll_now(&mut inner);
        assert_eq!(inner.snapshot.agents[0].model, None);
        writeln!(file, "{}", &good[good.len() / 2..]).unwrap();
        poll_now(&mut inner);
        assert_eq!(inner.snapshot.agents[0].model.as_deref(), Some("actual"));
        writeln!(file, "{}", "x".repeat(MAX_LINE * 3)).unwrap();
        writeln!(
            file,
            "{}",
            record(&inner, "event_msg", 2, json!({"type":"task_complete"}))
        )
        .unwrap();
        for _ in 0..5 {
            poll_now(&mut inner);
            assert!(inner.tails[0].partial.len() <= MAX_LINE);
        }
        assert_eq!(inner.snapshot.agents[0].status, AgentStatus::Completed);
    }
    #[test]
    fn large_forks_begin_in_a_bounded_live_window_and_polling_is_throttled() {
        let (_dir, mut inner) = fixture();
        root(&mut inner);
        let path = write_session(
            &inner,
            "child",
            &[metadata(&inner, "child", "root", Some(100))],
        );
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        for ordinal in 1..90 {
            writeln!(
                file,
                "{}",
                record(
                    &inner,
                    "response_item",
                    ordinal,
                    json!({"type":"reasoning","text":"x".repeat(MAX_LINE)})
                )
            )
            .unwrap();
        }
        writeln!(
            file,
            "{}",
            record(
                &inner,
                "turn_context",
                100,
                json!({"model":"actual-live-model"})
            )
        )
        .unwrap();
        let size = file.metadata().unwrap().len();
        poll_now(&mut inner);
        assert!(inner.tails[0].offset >= size - SCAN_BYTES as u64);
        let offset = inner.tails[0].offset;
        inner.poll();
        assert_eq!(inner.tails[0].offset, offset);
        for _ in 0..9 {
            poll_now(&mut inner);
        }
        assert_eq!(
            inner.snapshot.agents[0].model.as_deref(),
            Some("actual-live-model")
        );
        assert_eq!(inner.snapshot.agents[0].preview, "");
    }
    #[test]
    fn truncated_or_replaced_rollout_cannot_replay_or_import_another_session() {
        let (_dir, mut inner) = fixture();
        root(&mut inner);
        write_session(
            &inner,
            "child",
            &[
                metadata(&inner, "child", "root", None),
                record(&inner, "event_msg", 1, json!({"type":"task_started"})),
                record(&inner, "event_msg", 2, json!({"type":"task_complete"})),
            ],
        );
        poll_now(&mut inner);
        write_session(
            &inner,
            "child",
            &[
                metadata(&inner, "child", "root", None),
                record(&inner, "event_msg", 1, json!({"type":"task_started"})),
            ],
        );
        poll_now(&mut inner);
        assert_eq!(inner.snapshot.agents[0].status, AgentStatus::Completed);
        write_session(
            &inner,
            "child",
            &[
                metadata(&inner, "unrelated", "other-root", None),
                record(&inner, "turn_context", 100, json!({"model":"SECRET"})),
            ],
        );
        poll_now(&mut inner);
        assert_eq!(inner.snapshot.agents[0].model, None);
    }
    #[test]
    fn children_reset_when_root_changes_and_foreign_sender_is_rejected() {
        let (_dir, mut inner) = fixture();
        root(&mut inner);
        spawn(&mut inner, "child");
        inner.stream(&json!({"type":"thread.started","thread_id":"new-root"}));
        assert!(inner.snapshot.agents.is_empty());
        spawn(&mut inner, "child");
        assert!(inner.snapshot.agents.is_empty());
    }
    #[test]
    fn snapshot_and_card_text_are_bounded_and_terminal_safe() {
        let (_dir, mut inner) = fixture();
        root(&mut inner);
        let huge = "🦀".repeat(1000);
        for id in 0..MAX_AGENTS + 10 {
            if let Some(agent) = inner.agent(&format!("child{id}"), Some("root")) {
                agent.title = sanitize(&huge, 120);
                agent.preview = sanitize(&huge, 240);
                agent.model = nonempty(&huge, 120);
            }
        }
        inner.agent("child128", Some("root"));
        assert_eq!(inner.snapshot.agents.len(), MAX_AGENTS);
        assert_eq!(inner.snapshot.omitted, 10);
        inner.flush(true);
        assert!(fs::metadata(&inner.output).unwrap().len() < MAX_SNAPSHOT as u64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&inner.output).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            sanitize(
                "\u{1b}[31mTítulo\u{1b}[0m\n\u{202e} útil\u{1b}]52;c;SECRET\u{7}",
                120
            ),
            "Título útil"
        );
        assert_eq!(inner.snapshot.agents[0].title.chars().count(), 120);
    }
    #[test]
    fn finishing_root_does_not_invent_child_success_and_off_reporter_does_nothing() {
        let (_dir, mut inner) = fixture();
        root(&mut inner);
        spawn(&mut inner, "child");
        let output = inner.output.clone();
        let mut reporter = Reporter { inner: Some(inner) };
        reporter.finish(AgentStatus::Completed);
        let snapshot: ActivitySnapshot =
            serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
        assert_eq!(snapshot.agents[0].status, AgentStatus::Unknown);
        let stopped = snapshot.agents[0].finished_at.unwrap();
        assert_eq!(
            snapshot.agents[0].elapsed_at(stopped),
            snapshot.agents[0].elapsed_at(stopped + chrono::Duration::hours(1))
        );
        let mut off = Reporter { inner: None };
        off.update(Some("INVALID"));
        off.finish(AgentStatus::Failed);
        assert!(off.inner.is_none());
    }
    #[test]
    fn claude_cards_use_agent_tool_ids_and_actual_child_model_without_thinking() {
        let (_dir, mut inner) = fixture();
        inner.client = Backend::Claude;
        inner.snapshot.supported = true;
        inner.stream(&json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool-child","name":"Agent","input":{"description":"Corrigir layout","model":"requested-alias"}}]}}));
        assert_eq!(inner.snapshot.agents[0].model, None);
        inner.stream(&json!({"type":"assistant","parent_tool_use_id":"tool-child","message":{"model":"claude-observed","content":[{"type":"thinking","thinking":"SECRET"},{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}));
        assert_eq!(
            inner.snapshot.agents[0].model.as_deref(),
            Some("claude-observed")
        );
        assert_eq!(inner.snapshot.agents[0].preview, "Bash · cargo test");
        inner.stream(&json!({"type":"system","subtype":"task_started","task_type":"local_agent","task_id":"task","tool_use_id":"tool-child","description":"Corrigir layout"}));
        inner.stream(&json!({"type":"system","subtype":"task_notification","task_id":"task","status":"completed","summary":"Ajustado"}));
        assert_eq!(inner.snapshot.agents.len(), 1);
        assert_eq!(inner.snapshot.agents[0].status, AgentStatus::Completed);
        inner.stream(&json!({"type":"system","subtype":"task_started","task_type":"local_bash","task_id":"bash","description":"Não é agente"}));
        assert_eq!(inner.snapshot.agents.len(), 1);
    }
    #[test]
    fn unsupported_adapters_do_not_invent_cards_or_read_sessions() {
        for client in [Backend::Cursor, Backend::Grok] {
            let dir = tempfile::tempdir().unwrap();
            let mut inner = Inner::new(
                dir.path().join("snapshot"),
                client,
                dir.path().join("missing"),
            );
            root(&mut inner);
            spawn(&mut inner, "fake");
            poll_now(&mut inner);
            inner.flush(true);
            assert!(!inner.snapshot.supported);
            assert!(inner.snapshot.agents.is_empty());
            assert!(inner.inspected.is_empty());
        }
    }
}
