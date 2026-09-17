//! Job-owned control transport. Native agent messages are confirmed only by tool events.
//!
//! Codex V2 deliberately rejects direct turn input to children. We steer the owning
//! orchestrator and observe its native send/interrupt/followup calls instead.
//! Protocol verified against local 0.153.4 generate-ts schemas and official sources:
//! `app-server/src/request_processors/thread_input.rs`,
//! `core/src/tools/handlers/multi_agents_v2/{message_tool,interrupt_agent}.rs`.
use crate::{
    model::Tokens,
    providers,
    runner::{ChildGuard, InterruptGuard, Outcome, Request},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tempfile::TempDir;

pub(crate) const ENV_PATH: &str = "STACKPULSE_CONTROL_DIR";
const MAX_REQUESTS: usize = 128;
const MAX_MESSAGE: usize = 16 * 1024;
const MAX_STATE: u64 = 1024 * 1024;
const RPC_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlCapability {
    pub can_send: bool,
    pub can_interrupt: bool,
    pub note: String,
}
impl ControlCapability {
    pub fn unavailable(note: impl Into<String>) -> Self {
        Self {
            can_send: false,
            can_interrupt: false,
            note: note.into(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlStatus {
    Queued,
    NotifyingOrchestrator,
    OrchestratorNotified,
    WaitingForAgent,
    Delivered,
    Interrupting,
    Interrupted,
    Redirected,
    Failed,
    Unsupported,
    Unconfirmed,
}
impl ControlStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Na fila",
            Self::NotifyingOrchestrator => "Notificando orquestrador",
            Self::OrchestratorNotified => "Orquestrador notificado",
            Self::WaitingForAgent => "Aguardando repasse",
            Self::Delivered => "Repasse confirmado",
            Self::Interrupting => "Interrupção solicitada",
            Self::Interrupted => "Interrupção confirmada",
            Self::Redirected => "Redirecionamento confirmado",
            Self::Failed => "Falhou",
            Self::Unsupported => "Indisponível",
            Self::Unconfirmed => "Sem confirmação",
        }
    }
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Delivered
                | Self::Redirected
                | Self::Failed
                | Self::Unsupported
                | Self::Unconfirmed
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlReceipt {
    pub request_id: String,
    pub status: ControlStatus,
    pub detail: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ControlRequest {
    request_id: String,
    agent_id: String,
    message: String,
    redirect: bool,
}
impl ControlRequest {
    fn validate(&self) -> Result<()> {
        ensure!(
            uuid::Uuid::parse_str(&self.request_id).is_ok(),
            "Identificador da orientação inválido"
        );
        ensure!(
            valid_id(&self.agent_id),
            "Identificador do subagente inválido"
        );
        ensure!(!self.message.trim().is_empty(), "Escreva a orientação");
        ensure!(
            self.message.len() <= MAX_MESSAGE,
            "Orientação maior que 16 KiB"
        );
        Ok(())
    }
    fn marker(&self) -> String {
        format!("[StackPulse:{}]", self.request_id)
    }
    fn orchestrator_input(&self) -> String {
        let action = if self.redirect {
            "First interrupt only this agent's current task using interrupt_agent; then use followup_task on the same agent with the exact guidance below. Do not interrupt the main task or other agents."
        } else {
            "Use send_message (or send_input on an older runtime) to forward the exact guidance below to this existing agent."
        };
        format!(
            "StackPulse user intervention. Target existing subagent ID: {}. {}\nPreserve the main objective. Do not create a replacement agent. Keep the marker in the forwarded message so StackPulse can correlate the tool acknowledgement. If the target or operation is unavailable, report that honestly. A textual claim of success is not a tool acknowledgement.\nGuidance to forward:\n{}\n{}",
            self.agent_id,
            action,
            self.marker(),
            self.message
        )
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ControlSnapshot {
    capability: ControlCapability,
    receipts: Vec<ControlReceipt>,
}

/// One runtime request; the opaque local ID never contains a filesystem path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ApprovalRequest {
    pub request_id: String,
    pub method: String,
    pub params: Value,
}

/// Owned by Job, never reused across runs or inherited by the native provider.
pub(crate) struct Mailbox(TempDir, [u8; 32]);
impl Mailbox {
    pub(crate) fn new() -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("stackpulse-control-")
            .tempdir()?;
        fs::create_dir(directory.path().join("requests"))?;
        fs::create_dir(directory.path().join("approvals"))?;
        let mut key = [0_u8; 32];
        key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        let mailbox = Self(directory, key);
        write_state(
            mailbox.path(),
            &ControlSnapshot {
                capability: ControlCapability::unavailable(
                    "Aguardando canal do runtime; CLIs sem canal não recebem orientações ao vivo.",
                ),
                receipts: Vec::new(),
            },
        )?;
        Ok(mailbox)
    }
    pub(crate) fn approval_key(&self) -> &[u8; 32] {
        &self.1
    }
    pub(crate) fn path(&self) -> &Path {
        self.0.path()
    }
    pub(crate) fn capability(&self) -> ControlCapability {
        self.snapshot()
            .map(|s| s.capability)
            .unwrap_or_else(|| ControlCapability::unavailable("Canal de controle indisponível."))
    }
    pub(crate) fn receipts(&self) -> Vec<ControlReceipt> {
        self.snapshot().map(|s| s.receipts).unwrap_or_default()
    }
    fn snapshot(&self) -> Option<ControlSnapshot> {
        read_json(&self.path().join("state.json"), MAX_STATE).ok()
    }
    pub(crate) fn approvals(&self) -> Vec<ApprovalRequest> {
        let Ok(entries) = fs::read_dir(self.path().join("approvals")) else {
            return Vec::new();
        };
        let mut requests: Vec<ApprovalRequest> = entries
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| read_json(&entry.path(), MAX_STATE).ok())
            .filter(|request: &ApprovalRequest| {
                !self
                    .path()
                    .join("approvals")
                    .join(format!("{}.decision", request.request_id))
                    .exists()
            })
            .collect();
        requests.sort_by(|a, b| a.request_id.cmp(&b.request_id));
        requests
    }
    pub(crate) fn decide_approval(&self, request: &ApprovalRequest, accept: bool) -> Result<()> {
        let request_id = &request.request_id;
        ensure!(
            uuid::Uuid::parse_str(request_id).is_ok(),
            "Identificador de aprovação inválido"
        );
        let dir = self.path().join("approvals");
        let current: ApprovalRequest =
            read_json(&dir.join(format!("{request_id}.json")), MAX_STATE)
                .context("O pedido de permissão já encerrou")?;
        ensure!(
            &current == request,
            "O pedido mudou; revise os detalhes novamente"
        );
        let mut file = tempfile::NamedTempFile::new_in(&dir)?;
        serde_json::to_writer(
            &mut file,
            &ApprovalDecision::signed(&self.1, request, accept),
        )?;
        file.persist_noclobber(dir.join(format!("{request_id}.decision")))?;
        Ok(())
    }
    pub(crate) fn submit(
        &self,
        request_id: &str,
        agent_id: &str,
        message: &str,
        redirect: bool,
    ) -> Result<()> {
        let request = ControlRequest {
            request_id: request_id.into(),
            agent_id: agent_id.into(),
            message: message.into(),
            redirect,
        };
        request.validate()?;
        let path = self
            .path()
            .join("requests")
            .join(format!("{}.json", request.request_id));
        // A caller can retry an uncertain write, but cannot reuse an ID for new work.
        if path.exists() {
            let existing: ControlRequest = read_json(&path, (MAX_MESSAGE + 2048) as u64)?;
            ensure!(
                existing == request,
                "Esse identificador já pertence a outra orientação"
            );
            return Ok(());
        }
        let capability = self.capability();
        ensure!(
            if redirect {
                capability.can_interrupt
            } else {
                capability.can_send
            },
            "{}",
            capability.note
        );
        ensure!(
            fs::read_dir(self.path().join("requests"))?
                .take(MAX_REQUESTS + 1)
                .count()
                < MAX_REQUESTS,
            "Limite de orientações desta execução atingido"
        );
        let mut pending = tempfile::NamedTempFile::new_in(self.path().join("requests"))?;
        serde_json::to_writer(&mut pending, &request)?;
        pending.as_file().sync_all()?;
        pending
            .persist_noclobber(path)
            .context("Não foi possível enfileirar a orientação")?;
        Ok(())
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path, limit: u64) -> Result<T> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Registro de controle excede o limite"
    );
    Ok(serde_json::from_slice(&bytes)?)
}
fn write_state(path: &Path, state: &ControlSnapshot) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path)?;
    serde_json::to_writer(&mut file, state)?;
    ensure!(
        file.as_file().metadata()?.len() <= MAX_STATE,
        "Estado de controle excede o limite"
    );
    file.persist(path.join("state.json"))?;
    Ok(())
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b))
}
pub(crate) fn requested() -> bool {
    std::env::var_os(ENV_PATH).is_some_and(|v| !v.is_empty())
}

pub(crate) fn unsupported(client: crate::client::Backend) {
    if let Some(path) = std::env::var_os(ENV_PATH).filter(|p| !p.is_empty()) {
        let _ = write_state(
            Path::new(&path),
            &ControlSnapshot {
                capability: ControlCapability::unavailable(format!(
                    "{} não oferece um canal de orientação ao vivo no modo de execução atual.",
                    client.label()
                )),
                receipts: Vec::new(),
            },
        );
    }
}

struct PendingControl {
    request: ControlRequest,
    notified: bool,
    interrupted: bool,
    forwarded: bool,
    finished: bool,
}
struct Inbox {
    path: PathBuf,
    approval_key: [u8; 32],
    snapshot: ControlSnapshot,
    seen: HashSet<String>,
    pending: Vec<PendingControl>,
    tool_calls: HashMap<String, (String, String)>,
    agent_aliases: HashMap<String, String>,
}
impl Inbox {
    fn open(path: PathBuf, approval_key: [u8; 32]) -> Result<Self> {
        ensure!(
            path.join("requests").is_dir(),
            "Canal privado da execução indisponível"
        );
        Ok(Self {
            path,
            approval_key,
            snapshot: ControlSnapshot {
                capability: ControlCapability::unavailable("Conectando ao runtime do Codex…"),
                receipts: Vec::new(),
            },
            seen: HashSet::new(),
            pending: Vec::new(),
            tool_calls: HashMap::new(),
            agent_aliases: HashMap::new(),
        })
    }
    fn receipt(&mut self, id: &str, status: ControlStatus, detail: impl Into<String>) {
        let receipt = ControlReceipt {
            request_id: id.into(),
            status,
            detail: detail
                .into()
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(500)
                .collect(),
        };
        if !self.snapshot.receipts.contains(&receipt) {
            self.snapshot.receipts.push(receipt);
        }
    }
    fn ready(&mut self) {
        self.snapshot.capability = ControlCapability {
            can_send: true, can_interrupt: true,
            note: "Via orquestrador: repasse e interrupção dependem de confirmação das ferramentas do Codex.".into(),
        };
    }
    fn requests(&mut self) -> Result<Vec<ControlRequest>> {
        let mut requests = Vec::new();
        for entry in fs::read_dir(self.path.join("requests"))?
            .take(MAX_REQUESTS * 2)
            .flatten()
        {
            let path = entry.path();
            if path.extension().is_none_or(|s| s != "json") {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if self.seen.contains(id) {
                continue;
            }
            let request: ControlRequest = read_json(&path, (MAX_MESSAGE + 2048) as u64)?;
            request.validate()?;
            ensure!(
                id == request.request_id,
                "Identificador de arquivo de controle divergente"
            );
            if self.seen.len() == MAX_REQUESTS {
                break;
            }
            self.seen.insert(id.into());
            self.receipt(
                id,
                ControlStatus::Queued,
                "Orientação recebida pelo runtime local.",
            );
            requests.push(request);
        }
        Ok(requests)
    }
    fn confirm_operation(&mut self, request_id: &str, interrupted: bool) {
        if let Some(pending) = self
            .pending
            .iter_mut()
            .find(|p| p.request.request_id == request_id && !p.finished)
        {
            if interrupted {
                pending.interrupted = true;
            } else {
                pending.forwarded = true;
            }
        }
        self.confirm_ready();
    }
    fn confirm_ready(&mut self) {
        let mut receipts = Vec::new();
        for pending in &mut self.pending {
            if pending.finished || !pending.notified {
                continue;
            }
            if pending.interrupted {
                receipts.push((
                    pending.request.request_id.clone(),
                    ControlStatus::Interrupted,
                    "O CLI confirmou a ferramenta de interrupção deste agente.",
                ));
            }
            if pending.forwarded && (!pending.request.redirect || pending.interrupted) {
                pending.finished = true;
                receipts.push((pending.request.request_id.clone(),
                    if pending.request.redirect { ControlStatus::Redirected } else { ControlStatus::Delivered },
                    "O CLI confirmou o repasse integral da orientação ao agente; isso não comprova que o trabalho foi concluído."));
            }
        }
        for (id, status, detail) in receipts {
            self.receipt(&id, status, detail);
        }
    }
    fn tool_event(&mut self, params: &Value, root: &str) {
        let item = &params["item"];
        // V2 emits this typed item only after a successful native operation.
        // Its id is the call_id; the whitelisted function arguments supply correlation.
        if params["threadId"] == root && item["type"] == "subAgentActivity" {
            if let Some((request_id, tool)) = item["id"]
                .as_str()
                .and_then(|id| self.tool_calls.remove(id))
            {
                let Some(pending) = self
                    .pending
                    .iter()
                    .find(|p| p.request.request_id == request_id)
                else {
                    return;
                };
                if item["agentThreadId"] != pending.request.agent_id {
                    return;
                }
                let kind = item["kind"].as_str().unwrap_or_default();
                if tool == "interruptAgent" && kind == "interrupted" {
                    self.confirm_operation(&request_id, true);
                } else if tool != "interruptAgent" && kind == "interacted" {
                    self.confirm_operation(&request_id, false);
                }
            }
            return;
        }
        if item["type"] != "collabAgentToolCall" || item["senderThreadId"] != root {
            return;
        }
        let ids = item["receiverThreadIds"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut confirmations = Vec::new();
        let mut failures = Vec::new();
        for pending in &mut self.pending {
            if pending.finished || !ids.iter().any(|id| id == &pending.request.agent_id) {
                continue;
            }
            let tool = item["tool"].as_str().unwrap_or_default();
            let matched_message = item["prompt"].as_str().is_some_and(|s| {
                s.contains(&pending.request.marker()) && s.contains(&pending.request.message)
            });
            let is_interrupt = pending.request.redirect
                && tool == "interruptAgent"
                && item["id"]
                    .as_str()
                    .and_then(|id| self.tool_calls.get(id))
                    .is_some_and(|(id, _)| id == &pending.request.request_id);
            if !matched_message && !is_interrupt {
                continue;
            }
            let target_status = item["agentsStates"][&pending.request.agent_id]["status"].as_str();
            if item["status"] == "failed"
                || item["status"] == "interrupted"
                || matches!(
                    target_status,
                    Some("not_found" | "notFound" | "errored" | "failed")
                )
            {
                pending.finished = true;
                failures.push(pending.request.request_id.clone());
            } else if item["status"] == "completed"
                && matches!(
                    target_status,
                    Some("pendingInit" | "running" | "completed" | "shutdown" | "interrupted")
                )
            {
                if is_interrupt {
                    confirmations.push((pending.request.request_id.clone(), true));
                }
                if matched_message && matches!(tool, "sendMessage" | "sendInput" | "followupTask") {
                    confirmations.push((pending.request.request_id.clone(), false));
                }
            }
        }
        for id in failures {
            self.receipt(
                &id,
                ControlStatus::Failed,
                "A ferramenta de controle do agente não concluiu a ação.",
            );
        }
        for (id, interrupt) in confirmations {
            self.confirm_operation(&id, interrupt);
        }
    }
    fn raw_tool_call(&mut self, params: &Value, root: &str) {
        if params["threadId"] != root {
            return;
        }
        let item = &params["item"];
        if item["type"] != "function_call" {
            return;
        }
        if item["namespace"]
            .as_str()
            .is_some_and(|s| s != "collaboration")
        {
            return;
        }
        let tool = match item["name"].as_str().unwrap_or_default() {
            "send_message" => "sendMessage",
            "followup_task" => "followupTask",
            "interrupt_agent" => "interruptAgent",
            _ => return,
        };
        let Some(call_id) = item["call_id"].as_str().filter(|s| valid_id(s)) else {
            return;
        };
        let Some(arguments) = item["arguments"]
            .as_str()
            // JSON may expand one accepted byte into a six-byte unicode escape.
            .filter(|s| s.len() <= MAX_MESSAGE * 6 + 8192)
        else {
            return;
        };
        let Ok(args) = serde_json::from_str::<Value>(arguments) else {
            return;
        };
        for pending in &self.pending {
            let target = args["target"].as_str().unwrap_or_default();
            if pending.finished
                || (target != pending.request.agent_id
                    && self.agent_aliases.get(target) != Some(&pending.request.agent_id))
            {
                continue;
            }
            let matched = if tool == "interruptAgent" {
                pending.request.redirect
            } else {
                args["message"].as_str().is_some_and(|s| {
                    s.contains(&pending.request.marker()) && s.contains(&pending.request.message)
                })
            };
            if matched && self.tool_calls.len() < MAX_REQUESTS * 3 {
                self.tool_calls.insert(
                    call_id.into(),
                    (pending.request.request_id.clone(), tool.into()),
                );
                break;
            }
        }
    }
    fn finish(&mut self, detail: &str) {
        let _ = fs::remove_dir_all(self.path.join("approvals"));
        self.snapshot.capability = ControlCapability::unavailable(
            "A execução terminou; o canal de controle está fechado.",
        );
        let ids: Vec<_> = self
            .pending
            .iter_mut()
            .filter(|p| !p.finished)
            .map(|p| {
                p.finished = true;
                p.request.request_id.clone()
            })
            .collect();
        for id in ids {
            self.receipt(&id, ControlStatus::Unconfirmed, detail);
        }
        // Include submissions racing with the last turn notification; none are replayed.
        if let Ok(requests) = self.requests() {
            for request in requests {
                self.receipt(&request.request_id, ControlStatus::Unconfirmed, detail);
            }
        }
        let _ = self.flush();
    }
    fn flush(&self) -> Result<()> {
        write_state(&self.path, &self.snapshot)
    }
}

// The key travels only through the UI child's anonymous stdin pipe. Bind the
// decision to every displayed field, not just an ID in a writable temp file.
#[derive(Serialize, Deserialize)]
struct ApprovalDecision {
    accept: bool,
    mac: [u8; 32],
}
impl ApprovalDecision {
    fn signed(key: &[u8; 32], request: &ApprovalRequest, accept: bool) -> Self {
        use sha2::{Digest, Sha256};
        let mut inner_pad = [0x36_u8; 64];
        let mut outer_pad = [0x5c_u8; 64];
        for i in 0..32 {
            inner_pad[i] ^= key[i];
            outer_pad[i] ^= key[i];
        }
        let mut inner = Sha256::new();
        inner.update(inner_pad);
        inner.update(serde_json::to_vec(&(request, accept)).expect("serializable approval"));
        let mut outer = Sha256::new();
        outer.update(outer_pad);
        outer.update(inner.finalize());
        Self {
            accept,
            mac: outer.finalize().into(),
        }
    }
    fn valid(&self, key: &[u8; 32], request: &ApprovalRequest) -> bool {
        let expected = Self::signed(key, request, self.accept);
        self.mac
            .iter()
            .zip(expected.mac)
            .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
            == 0
    }
}

struct PendingApprovals {
    directory: PathBuf,
    key: [u8; 32],
    requests: Vec<(Value, ApprovalRequest)>,
}
impl PendingApprovals {
    fn new(directory: PathBuf, key: [u8; 32]) -> Self {
        Self {
            directory,
            key,
            requests: Vec::new(),
        }
    }
    fn enqueue(
        &mut self,
        value: &Value,
        known: &HashSet<String>,
        items: &HashMap<String, Value>,
    ) -> Result<bool> {
        let method = value["method"].as_str().unwrap_or_default();
        if !matches!(
            method,
            "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "item/permissions/requestApproval"
                | "mcpServer/elicitation/request"
        ) || !value["params"]["threadId"]
            .as_str()
            .is_some_and(|id| known.contains(id))
            || self.requests.len() >= MAX_REQUESTS
            || self.requests.iter().any(|(id, _)| id == &value["id"])
        {
            return Ok(false);
        }
        let mut params = value["params"].clone();
        // Empty MCP forms are confirmations, including tool-call approvals.
        // Do not synthesize answers for forms requiring user data or URL flows.
        if method == "mcpServer/elicitation/request"
            && (params["mode"] != "form"
                || params["requestedSchema"]["type"] != "object"
                || !params["requestedSchema"]["properties"]
                    .as_object()
                    .is_some_and(|fields| fields.is_empty())
                || !(params["requestedSchema"]["required"].is_null()
                    || params["requestedSchema"]["required"]
                        .as_array()
                        .is_some_and(|fields| fields.is_empty())))
        {
            return Ok(false);
        }
        if let Some(item) = params["itemId"]
            .as_str()
            .and_then(|id| items.get(&format!("{}:{id}", params["threadId"])))
        {
            params["itemDetails"] = item.clone();
        }
        if method == "item/fileChange/requestApproval"
            && !params["itemDetails"]["changes"]
                .as_array()
                .is_some_and(|changes| !changes.is_empty())
        {
            return Ok(false);
        }
        let request = ApprovalRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            method: method.into(),
            params,
        };
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        serde_json::to_writer(&mut file, &request)?;
        ensure!(
            file.as_file().metadata()?.len() <= MAX_STATE,
            "Pedido de aprovação grande demais"
        );
        file.persist_noclobber(self.directory.join(format!("{}.json", request.request_id)))?;
        self.requests.push((value["id"].clone(), request));
        Ok(true)
    }
    fn response(request: &ApprovalRequest, accept: bool) -> Value {
        if request.method == "mcpServer/elicitation/request" {
            json!({"action": if accept { "accept" } else { "decline" },
                "content": if accept { json!({}) } else { Value::Null }})
        } else if request.method == "item/permissions/requestApproval" {
            let mut permissions = serde_json::Map::new();
            if accept {
                for key in ["network", "fileSystem"] {
                    if let Some(value) = request.params["permissions"]
                        .get(key)
                        .filter(|v| !v.is_null())
                    {
                        permissions.insert(key.into(), value.clone());
                    }
                }
            }
            json!({"permissions":permissions,"scope":"turn"})
        } else {
            json!({"decision":if accept { "accept" } else { "decline" }})
        }
    }
    fn remove_files(&self, request: &ApprovalRequest) {
        for suffix in ["json", "decision"] {
            let _ = fs::remove_file(
                self.directory
                    .join(format!("{}.{suffix}", request.request_id)),
            );
        }
    }
    fn resolve(&mut self, id: &Value) {
        if let Some(index) = self.requests.iter().position(|(rpc_id, _)| rpc_id == id) {
            let (_, request) = self.requests.remove(index);
            self.remove_files(&request);
        }
    }
    fn end_turn(&mut self, params: &Value, writer: &RpcWriter) -> Result<()> {
        let ids: Vec<_> = self
            .requests
            .iter()
            .filter(|(_, request)| {
                request.params["threadId"] == params["threadId"]
                    && request.params["turnId"] == params["turn"]["id"]
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some((_, request)) = self.requests.iter().find(|(rpc_id, _)| rpc_id == &id) {
                writer.send(json!({"id":id,"result":Self::response(request, false)}))?;
            }
            self.resolve(&id);
        }
        Ok(())
    }
    fn poll(&mut self, writer: &RpcWriter) -> Result<()> {
        let mut done = Vec::new();
        for (id, request) in &self.requests {
            let path = self
                .directory
                .join(format!("{}.decision", request.request_id));
            if path.exists() {
                let decision: Option<ApprovalDecision> = read_json(&path, 1024).ok();
                let Some(decision) = decision.filter(|d| d.valid(&self.key, request)) else {
                    let _ = fs::remove_file(path);
                    continue;
                };
                writer.send(json!({"id":id,"result":Self::response(request, decision.accept)}))?;
                done.push(id.clone());
            }
        }
        for id in done {
            self.resolve(&id);
        }
        Ok(())
    }
}
impl Drop for PendingApprovals {
    fn drop(&mut self) {
        for (_, request) in &self.requests {
            self.remove_files(request);
        }
    }
}

struct RpcWriter(mpsc::SyncSender<Vec<u8>>);
impl RpcWriter {
    fn start(mut stdin: ChildStdin) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(4);
        std::thread::spawn(move || {
            while let Ok(bytes) = rx.recv() {
                if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        });
        Self(tx)
    }
    fn send(&self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        self.0
            .try_send(bytes)
            .context("O canal de entrada do CLI está ocupado ou fechado")?;
        Ok(())
    }
}
fn send_rpc(stdin: &RpcWriter, id: u64, method: &str, params: Value) -> Result<()> {
    stdin.send(json!({"id":id,"method":method,"params":params}))
}
fn public_event(events: &mut impl FnMut(Option<&str>), value: &Value) {
    if let Ok(line) = serde_json::to_string(value) {
        events(Some(&line));
    }
}

/// Only UI-tracked Codex runs opt into this owned server. CLI `exec` stays unchanged.
pub(crate) fn execute(
    request: Request<'_>,
    mut progress: impl FnMut(&Outcome) -> Result<()>,
    mut events: impl FnMut(Option<&str>),
) -> Result<Outcome> {
    let path = PathBuf::from(std::env::var_os(ENV_PATH).context("Canal de controle ausente")?);
    let mut key = [0_u8; 32];
    std::io::stdin()
        .read_exact(&mut key)
        .context("Canal privado de aprovação ausente")?;
    let mut inbox = Inbox::open(path, key)?;
    inbox.flush()?;
    let result = execute_inner(&request, &mut inbox, &mut progress, &mut events);
    inbox.finish("A execução encerrou sem confirmação suficiente do repasse ao agente. A orientação não será reenviada automaticamente.");
    result
}

fn execute_inner(
    request: &Request<'_>,
    inbox: &mut Inbox,
    progress: &mut impl FnMut(&Outcome) -> Result<()>,
    events: &mut impl FnMut(Option<&str>),
) -> Result<Outcome> {
    ensure!(
        request.image.is_none() && request.output_schema.is_none(),
        "Este canal de controle atende execuções de equipe, não compilação de imagens/schema"
    );
    if let Some(team) = request.team {
        team.validate()?;
    }
    let mut command = Command::new(&request.settings.executable);
    command
        .current_dir(request.cwd)
        .env_remove(ENV_PATH)
        .env_remove(crate::activity::ENV_PATH)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let _profiles = providers::codex::configure_app_server(&mut command, request)?;
    let interrupted = Arc::new(AtomicBool::new(false));
    let _interrupt_guard = InterruptGuard(signal_hook::flag::register(
        signal_hook::consts::SIGINT,
        interrupted.clone(),
    )?);
    let mut child = ChildGuard(
        command
            .spawn()
            .context("Não foi possível iniciar o app-server privado do Codex")?,
    );
    let stdin = RpcWriter::start(child.0.stdin.take().context("stdin indisponível")?);
    let stdout = child.0.stdout.take().context("stdout indisponível")?;
    let (tx, rx) = mpsc::sync_channel(64);
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            // Bound allocation before decoding, including malformed unterminated lines.
            let mut bytes = Vec::new();
            let result = reader
                .by_ref()
                .take(4_000_001)
                .read_until(b'\n', &mut bytes);
            match result {
                Ok(0) => break,
                Ok(_) if bytes.len() > 4_000_000 => {
                    let _ = tx.send(Err(std::io::Error::other(
                        "Evento do app-server excede 4 MB",
                    )));
                    break;
                }
                Ok(_) => {
                    if tx.send(Ok(bytes)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(error));
                    break;
                }
            }
        }
    });
    send_rpc(
        &stdin,
        1,
        "initialize",
        json!({"clientInfo":{"name":"stackpulse","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true,"requestAttestation":false}}),
    )?;
    let mut outcome = Outcome::default();
    let mut root = None::<String>;
    let mut root_turn = None::<String>;
    let mut known = HashSet::<String>::new();
    let mut next_id = 4_u64;
    let mut pending_rpc: HashMap<u64, (String, Instant)> = HashMap::new();
    let mut approvals = PendingApprovals::new(inbox.path.join("approvals"), inbox.approval_key);
    let mut approval_items = HashMap::<String, Value>::new();
    let deadline = Instant::now() + Duration::from_secs(request.timeout_secs);
    let startup_deadline = Instant::now() + RPC_TIMEOUT;
    let mut initialized_turn = false;
    loop {
        if interrupted.load(Ordering::Relaxed) || Instant::now() >= deadline {
            outcome.interrupted = interrupted.load(Ordering::Relaxed);
            outcome.timed_out = !outcome.interrupted;
            child.stop();
            break;
        }
        ensure!(
            initialized_turn || Instant::now() < startup_deadline,
            "O app-server não confirmou o início da execução em 30 segundos"
        );
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(bytes) => {
                let bytes = bytes?;
                let value: Value =
                    serde_json::from_slice(&bytes).context("Resposta inválida do app-server")?;
                if let Some(id) = value["id"]
                    .as_u64()
                    .filter(|_| value.get("method").is_none())
                {
                    if id <= 3 && value.get("error").is_some() {
                        bail!(
                            "App-server: {}",
                            value["error"]["message"]
                                .as_str()
                                .unwrap_or("requisição rejeitada")
                        );
                    }
                    match id {
                        1 => {
                            stdin.send(json!({"method":"initialized"}))?;
                            send_rpc(
                                &stdin,
                                2,
                                "thread/start",
                                json!({
                                    "model": (request.model != "default").then_some(request.model),
                                    "modelProvider": (request.provider != "default").then_some(request.provider),
                                    "cwd": request.cwd, "sandbox": request.sandbox, "approvalPolicy": if request.sandbox == "danger-full-access" { "never" } else { "on-request" }, "approvalsReviewer":"user",
                                    "ephemeral":false,"experimentalRawEvents":true
                                }),
                            )?;
                        }
                        2 => {
                            let id = value["result"]["thread"]["id"]
                                .as_str()
                                .filter(|id| valid_id(id))
                                .context("App-server não retornou a conversa criada")?
                                .to_owned();
                            root = Some(id.clone());
                            known.insert(id.clone());
                            outcome.thread_id = Some(id.clone());
                            outcome.observed_model =
                                value["result"]["model"].as_str().map(str::to_owned);
                            public_event(events, &json!({"type":"thread.started","thread_id":id}));
                            progress(&outcome)?;
                            send_rpc(
                                &stdin,
                                3,
                                "turn/start",
                                json!({"threadId":id,"input":[{"type":"text","text":request.prompt,"text_elements":[]}], "effort":(request.effort != "default").then_some(request.effort)}),
                            )?;
                        }
                        3 => {
                            let id = value["result"]["turn"]["id"]
                                .as_str()
                                .filter(|id| valid_id(id))
                                .context("App-server não retornou o turno iniciado")?;
                            root_turn = Some(id.into());
                            initialized_turn = true;
                            inbox.ready();
                            inbox.flush()?;
                        }
                        _ => {
                            if let Some((request_id, _)) = pending_rpc.remove(&id) {
                                if value.get("error").is_some() {
                                    inbox.receipt(
                                        &request_id,
                                        ControlStatus::Failed,
                                        format!(
                                            "Orquestrador não aceitou a orientação: {}",
                                            value["error"]["message"]
                                                .as_str()
                                                .unwrap_or("requisição rejeitada")
                                        ),
                                    );
                                    if let Some(p) = inbox
                                        .pending
                                        .iter_mut()
                                        .find(|p| p.request.request_id == request_id)
                                    {
                                        p.finished = true;
                                    }
                                } else if value["result"]["turnId"].as_str() == root_turn.as_deref()
                                {
                                    if let Some(p) = inbox
                                        .pending
                                        .iter_mut()
                                        .find(|p| p.request.request_id == request_id)
                                    {
                                        p.notified = true;
                                    }
                                    inbox.receipt(&request_id, ControlStatus::OrchestratorNotified, "O CLI aceitou a orientação no turno ativo do orquestrador.");
                                    inbox.receipt(&request_id, ControlStatus::WaitingForAgent, "Aguardando confirmação da ferramenta de repasse ao subagente.");
                                    inbox.confirm_ready();
                                } else {
                                    inbox.receipt(&request_id, ControlStatus::Unconfirmed, "Resposta do CLI não confirmou o turno esperado; a orientação não será reenviada.");
                                    if let Some(p) = inbox
                                        .pending
                                        .iter_mut()
                                        .find(|p| p.request.request_id == request_id)
                                    {
                                        p.finished = true;
                                    }
                                }
                                inbox.flush()?;
                            }
                        }
                    }
                } else if value.get("id").is_some() && value.get("method").is_some() {
                    if !approvals.enqueue(&value, &known, &approval_items)? {
                        stdin.send(json!({"id":value["id"],"error":{"code":-32000,"message":"Interação não suportada ou conversa desconhecida; solicitação não aprovada."}}))?;
                    }
                } else if let Some(method) = value["method"].as_str() {
                    let params = &value["params"];
                    observe_tree(method, params, &mut known);
                    if method == "serverRequest/resolved" {
                        approvals.resolve(&params["requestId"]);
                    }
                    if method == "turn/completed" {
                        approvals.end_turn(params, &stdin)?;
                    }
                    if method == "item/completed" {
                        if let Some(id) = params["item"]["id"].as_str() {
                            approval_items.remove(&format!("{}:{id}", params["threadId"]));
                        }
                    }
                    if matches!(method, "item/started" | "item/updated")
                        && params["item"]["type"] == "fileChange"
                        && params["threadId"]
                            .as_str()
                            .is_some_and(|id| known.contains(id))
                        && let Some(id) = params["item"]["id"].as_str()
                        && approval_items.len() < 512
                    {
                        approval_items.insert(
                            format!("{}:{id}", params["threadId"]),
                            params["item"].clone(),
                        );
                    }
                    if method == "item/completed"
                        && params["item"]["type"] == "subAgentActivity"
                        && params["threadId"]
                            .as_str()
                            .is_some_and(|id| known.contains(id))
                        && let Some(id) = params["item"]["agentThreadId"]
                            .as_str()
                            .filter(|id| known.contains(*id))
                        && let Some(path) = params["item"]["agentPath"]
                            .as_str()
                            .filter(|p| valid_id(p) && p.starts_with("/root/"))
                        && inbox.agent_aliases.len() < 512
                    {
                        inbox.agent_aliases.insert(path.into(), id.into());
                        inbox
                            .agent_aliases
                            .insert(path.trim_start_matches("/root/").into(), id.into());
                    }
                    // Raw function-call arguments are used solely for acknowledgement
                    // correlation. Raw messages/reasoning never reach UI observers.
                    if method != "rawResponseItem/completed" && method != "rawResponse/completed" {
                        public_event(events, &value);
                    }
                    if let Some(root) = &root {
                        if method == "rawResponseItem/completed" {
                            inbox.raw_tool_call(params, root);
                        }
                        if method == "item/completed" {
                            inbox.tool_event(params, root);
                            inbox.flush()?;
                        }
                        if params["threadId"] == *root {
                            update_outcome(&mut outcome, method, params);
                            if method == "turn/started" {
                                root_turn = params["turn"]["id"].as_str().map(str::to_owned);
                            }
                            if matches!(method, "thread/tokenUsage/updated" | "turn/completed") {
                                progress(&outcome)?;
                            }
                            if method == "turn/completed" {
                                outcome.exit_code = Some(if outcome.failed { 1 } else { 0 });
                                break;
                            }
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                outcome.failed = true;
                outcome.error_message =
                    Some("O app-server fechou o canal antes da confirmação de conclusão.".into());
                outcome.exit_code = child.0.try_wait()?.and_then(|s| s.code()).or(Some(1));
                break;
            }
        }
        approvals.poll(&stdin)?;
        let expired: Vec<_> = pending_rpc
            .iter()
            .filter(|(_, (_, at))| at.elapsed() >= RPC_TIMEOUT)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if let Some((request_id, _)) = pending_rpc.remove(&id) {
                inbox.receipt(&request_id, ControlStatus::Unconfirmed, "O CLI não confirmou recebimento em 30 segundos. Nenhuma repetição automática foi feita.");
                if let Some(p) = inbox
                    .pending
                    .iter_mut()
                    .find(|p| p.request.request_id == request_id)
                {
                    p.finished = true;
                }
                inbox.flush()?;
            }
        }
        if initialized_turn {
            for control in inbox.requests()? {
                let id = control.request_id.clone();
                if root.as_deref() == Some(&control.agent_id) || !known.contains(&control.agent_id)
                {
                    inbox.receipt(
                        &id,
                        ControlStatus::Failed,
                        "O subagente não foi confirmado como membro desta execução.",
                    );
                } else if inbox
                    .pending
                    .iter()
                    .any(|p| !p.finished && p.request.agent_id == control.agent_id)
                {
                    inbox.receipt(
                        &id,
                        ControlStatus::Failed,
                        "Este agente já tem uma orientação aguardando confirmação.",
                    );
                } else {
                    let method_id = next_id;
                    next_id += 1;
                    let rpc = json!({"threadId":root,"expectedTurnId":root_turn,"clientUserMessageId":id,"input":[{"type":"text","text":control.orchestrator_input(),"text_elements":[]}]});
                    inbox.receipt(
                        &id,
                        ControlStatus::NotifyingOrchestrator,
                        "Enviando orientação ao orquestrador desta execução.",
                    );
                    if control.redirect {
                        inbox.receipt(&id, ControlStatus::Interrupting, "Foi solicitada interrupção e nova orientação apenas para este agente; aguardando o orquestrador.");
                    }
                    inbox.pending.push(PendingControl {
                        request: control,
                        notified: false,
                        interrupted: false,
                        forwarded: false,
                        finished: false,
                    });
                    inbox.flush()?;
                    if let Err(error) = send_rpc(&stdin, method_id, "turn/steer", rpc) {
                        inbox.receipt(
                            &id,
                            ControlStatus::Failed,
                            format!("Não foi possível enviar a orientação: {error}"),
                        );
                        if let Some(p) = inbox
                            .pending
                            .iter_mut()
                            .find(|p| p.request.request_id == id)
                        {
                            p.finished = true;
                        }
                    } else {
                        pending_rpc.insert(method_id, (id, Instant::now()));
                    }
                }
                inbox.flush()?;
            }
        }
        events(None);
    }
    drop(stdin);
    // The server is per-job. Closing its input gracefully releases the owned tree;
    // the guard only uses a process-group signal for whole-job cancellation/cleanup.
    let until = Instant::now() + Duration::from_millis(500);
    while child.0.try_wait()?.is_none() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    if child.0.try_wait()?.is_none() {
        child.stop();
    }
    if outcome.exit_code.is_none() {
        outcome.exit_code = child.0.try_wait()?.and_then(|s| s.code());
    }
    progress(&outcome)?;
    Ok(outcome)
}

fn observe_tree(method: &str, params: &Value, known: &mut HashSet<String>) {
    if known.len() >= 256 {
        return;
    }
    if method == "thread/started" {
        let t = &params["thread"];
        if t["parentThreadId"]
            .as_str()
            .is_some_and(|id| known.contains(id))
            && let Some(id) = t["id"].as_str().filter(|id| valid_id(id))
        {
            known.insert(id.into());
        }
    } else if matches!(method, "item/started" | "item/completed") {
        let i = &params["item"];
        if method == "item/completed"
            && i["type"] == "collabAgentToolCall"
            && i["tool"] == "spawnAgent"
            && i["status"] == "completed"
            && i["senderThreadId"]
                .as_str()
                .is_some_and(|id| known.contains(id))
        {
            if let Some(ids) = i["receiverThreadIds"].as_array() {
                for id in ids
                    .iter()
                    .take(128)
                    .filter_map(Value::as_str)
                    .filter(|id| valid_id(id))
                {
                    if matches!(
                        i["agentsStates"][id]["status"].as_str(),
                        Some("pendingInit" | "running" | "completed")
                    ) {
                        known.insert(id.into());
                    }
                }
            }
        } else if i["type"] == "subAgentActivity"
            && i["kind"] == "started"
            && params["threadId"]
                .as_str()
                .is_some_and(|id| known.contains(id))
            && let Some(id) = i["agentThreadId"].as_str().filter(|id| valid_id(id))
        {
            known.insert(id.into());
        }
    }
}
fn update_outcome(outcome: &mut Outcome, method: &str, params: &Value) {
    match method {
        "thread/tokenUsage/updated" => {
            let usage = &params["tokenUsage"]["total"];
            if usage["inputTokens"].is_u64() && usage["outputTokens"].is_u64() {
                let tokens = Tokens {
                    input_tokens: usage["inputTokens"].as_u64().unwrap_or_default(),
                    output_tokens: usage["outputTokens"].as_u64().unwrap_or_default(),
                    cached_input_tokens: usage["cachedInputTokens"].as_u64().unwrap_or_default(),
                    cache_write_input_tokens: usage["cacheWriteInputTokens"]
                        .as_u64()
                        .unwrap_or_default(),
                    reasoning_output_tokens: usage["reasoningOutputTokens"]
                        .as_u64()
                        .unwrap_or_default(),
                };
                if tokens.valid() {
                    outcome.reported_tokens = Some(tokens);
                    outcome.reported_scope = Some("root_only".into());
                } else {
                    outcome.invalid_events += 1;
                }
            }
        }
        "item/completed" if params["item"]["type"] == "agentMessage" => {
            if let Some(text) = params["item"]["text"].as_str() {
                outcome.final_message = text.into();
            }
        }
        "turn/completed" => match params["turn"]["status"].as_str() {
            Some("completed") => outcome.completed_turns += 1,
            Some("interrupted") => outcome.interrupted = true,
            _ => {
                outcome.failed = true;
                outcome.error_message = params["turn"]["error"]["message"]
                    .as_str()
                    .map(str::to_owned);
            }
        },
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ROOT: &str = "00000000-0000-4000-8000-000000000001";
    const CHILD: &str = "00000000-0000-4000-8000-000000000002";

    fn pending(inbox: &mut Inbox, redirect: bool, notified: bool) -> ControlRequest {
        let request = ControlRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            agent_id: CHILD.into(),
            message: "Revise somente a validação de entradas.".into(),
            redirect,
        };
        inbox.pending.push(PendingControl {
            request: request.clone(),
            notified,
            interrupted: false,
            forwarded: false,
            finished: false,
        });
        request
    }
    fn v2_call(inbox: &mut Inbox, request: &ControlRequest, name: &str, message: &str) {
        inbox.raw_tool_call(&json!({"threadId":ROOT,"item":{"type":"function_call","namespace":"collaboration","name":name,"call_id":name,
            "arguments":json!({"target":CHILD,"message":message}).to_string()}}), ROOT);
        inbox.tool_event(&json!({"threadId":ROOT,"item":{"type":"subAgentActivity","id":name,"agentThreadId":CHILD,
            "kind":if name == "interrupt_agent" { "interrupted" } else { "interacted" }}}), ROOT);
        assert!(!request.request_id.is_empty());
    }

    #[test]
    fn mailbox_is_disabled_until_runtime_ack_and_idempotent_after_submit() {
        let mailbox = Mailbox::new().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        assert!(mailbox.submit(&id, CHILD, "orientação", false).is_err());
        let mut inbox = Inbox::open(mailbox.path().into(), *mailbox.approval_key()).unwrap();
        inbox.ready();
        inbox.flush().unwrap();
        mailbox.submit(&id, CHILD, "orientação", false).unwrap();
        mailbox.submit(&id, CHILD, "orientação", false).unwrap();
        assert!(
            mailbox
                .submit(&id, CHILD, "outra orientação", false)
                .is_err()
        );
        assert_eq!(inbox.requests().unwrap().len(), 1);
        assert!(inbox.requests().unwrap().is_empty());
        assert!(
            mailbox
                .submit("../../outside", CHILD, "orientação", false)
                .is_err()
        );
        assert!(
            mailbox
                .submit(
                    &uuid::Uuid::new_v4().to_string(),
                    CHILD,
                    &"a".repeat(MAX_MESSAGE + 1),
                    false
                )
                .is_err()
        );
    }

    #[test]
    fn guidance_requires_exact_message_target_and_native_ack() {
        let mailbox = Mailbox::new().unwrap();
        let mut inbox = Inbox::open(mailbox.path().into(), *mailbox.approval_key()).unwrap();
        let request = pending(&mut inbox, false, true);
        v2_call(&mut inbox, &request, "send_message", &request.marker());
        assert!(
            !inbox.pending[0].finished,
            "a marker alone does not prove guidance delivery"
        );
        let text = format!("{}\n{}", request.marker(), request.message);
        inbox.raw_tool_call(&json!({"threadId":ROOT,"item":{"type":"function_call","namespace":"collaboration","name":"send_message","call_id":"wrong-target",
            "arguments":json!({"target":"foreign","message":text}).to_string()}}), ROOT);
        inbox.tool_event(&json!({"threadId":ROOT,"item":{"type":"subAgentActivity","id":"wrong-target","agentThreadId":CHILD,"kind":"interacted"}}), ROOT);
        assert!(!inbox.pending[0].finished);
        v2_call(&mut inbox, &request, "send_message", &text);
        assert_eq!(
            inbox.snapshot.receipts.last().unwrap().status,
            ControlStatus::Delivered
        );
    }

    #[test]
    fn redirect_needs_both_native_operations_and_root_ack() {
        let mailbox = Mailbox::new().unwrap();
        let mut inbox = Inbox::open(mailbox.path().into(), *mailbox.approval_key()).unwrap();
        let request = pending(&mut inbox, true, false);
        let text = format!("{}\n{}", request.marker(), request.message);
        v2_call(&mut inbox, &request, "followup_task", &text);
        assert!(!inbox.pending[0].finished);
        v2_call(&mut inbox, &request, "interrupt_agent", "");
        assert!(!inbox.pending[0].finished);
        inbox.pending[0].notified = true;
        inbox.confirm_ready();
        assert_eq!(
            inbox.snapshot.receipts.last().unwrap().status,
            ControlStatus::Redirected
        );
    }

    #[test]
    fn completed_tool_with_missing_target_is_not_delivery() {
        let mailbox = Mailbox::new().unwrap();
        let mut inbox = Inbox::open(mailbox.path().into(), *mailbox.approval_key()).unwrap();
        let request = pending(&mut inbox, false, true);
        inbox.tool_event(&json!({"item":{"type":"collabAgentToolCall","senderThreadId":ROOT,"receiverThreadIds":[CHILD],"tool":"sendMessage","status":"completed",
            "prompt":format!("{}\n{}",request.marker(),request.message),"agentsStates":{CHILD:{"status":"notFound"}}}}), ROOT);
        assert_eq!(
            inbox.snapshot.receipts.last().unwrap().status,
            ControlStatus::Failed
        );
    }

    #[test]
    fn references_and_foreign_parents_do_not_create_membership() {
        let mut known = HashSet::from([ROOT.into()]);
        observe_tree(
            "item/completed",
            &json!({"item":{"type":"collabAgentToolCall","senderThreadId":ROOT,"receiverThreadIds":[CHILD],"tool":"sendMessage","status":"completed"}}),
            &mut known,
        );
        observe_tree(
            "thread/started",
            &json!({"thread":{"id":CHILD,"parentThreadId":"foreign"}}),
            &mut known,
        );
        assert!(!known.contains(CHILD));
        observe_tree(
            "thread/started",
            &json!({"thread":{"id":CHILD,"parentThreadId":ROOT}}),
            &mut known,
        );
        assert!(known.contains(CHILD));
        let mut known = HashSet::from([ROOT.into()]);
        observe_tree(
            "item/completed",
            &json!({"threadId":ROOT,"item":{"type":"subAgentActivity","id":"spawn","kind":"started","agentThreadId":CHILD,"agentPath":"/root/worker"}}),
            &mut known,
        );
        assert!(
            known.contains(CHILD),
            "V2 started uses the typed agent ID without needing a thread notification"
        );
    }

    #[test]
    fn root_usage_is_cumulative_not_added_twice_and_reasoning_is_ignored() {
        let mut outcome = Outcome::default();
        for input in [100, 120] {
            update_outcome(
                &mut outcome,
                "thread/tokenUsage/updated",
                &json!({"tokenUsage":{"total":{"inputTokens":input,"outputTokens":20,"cachedInputTokens":10,"reasoningOutputTokens":5}}}),
            );
        }
        assert_eq!(outcome.reported_tokens.unwrap().total(), 140);
        update_outcome(
            &mut outcome,
            "rawResponseItem/completed",
            &json!({"item":{"type":"reasoning","content":"private"}}),
        );
        assert!(outcome.final_message.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn no_policy_reaches_app_server() {
        let (outcome, _, _, _) = run_fixture("no_policy", false, 100);
        assert!(outcome.success(), "{outcome:?}");
    }

    #[cfg(unix)]
    fn run_fixture(
        mode: &str,
        redirect: bool,
        message_size: usize,
    ) -> (Outcome, Vec<ControlReceipt>, Vec<String>, Duration) {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("fake-app-server.json"),
            serde_json::to_vec(
                &json!({"mode":mode,"finish_after_control":true,"lifetime_seconds":1}),
            )
            .unwrap(),
        )
        .unwrap();
        let mailbox = Mailbox::new().unwrap();
        let mut inbox = Inbox::open(mailbox.path().into(), *mailbox.approval_key()).unwrap();
        let settings = crate::profiles::Settings {
            schema_version: 1,
            client: crate::client::Backend::Codex,
            provider: "openai".into(),
            model: "gpt-6-astra".into(),
            effort: "medium".into(),
            executable: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/fake-app-server.py"),
            providers_root: temp.path().into(),
            default_profile: None,
            max_agents: 3,
            ai_memory: false,
            ai_usagebar: false,
        };
        let prompt = "a".repeat(message_size);
        let request = Request {
            settings: &settings,
            cwd: temp.path(),
            model: "gpt-6-astra",
            effort: "medium",
            provider: "openai",
            prompt: &prompt,
            image: None,
            output_schema: None,
            sandbox: if mode == "no_policy" {
                "danger-full-access"
            } else {
                "workspace-write"
            },
            timeout_secs: 2,
            delegates: true,
            team: None,
        };
        let mut observed = Vec::new();
        let mut sent = false;
        let started = Instant::now();
        let outcome = execute_inner(&request, &mut inbox, &mut |_| Ok(()), &mut |line| {
            if mode.starts_with("approval_")
                && !sent
                && started.elapsed() > Duration::from_millis(200)
            {
                if let Some(approval) = mailbox.approvals().first() {
                    mailbox
                        .decide_approval(&approval, mode.ends_with("_accept"))
                        .unwrap();
                    sent = true;
                }
            }
            if let Some(line) = line {
                observed.push(line.to_owned());
                if !mode.starts_with("approval_")
                    && line.contains("thread/started")
                    && line.contains(CHILD)
                    && !sent
                {
                    mailbox
                        .submit(
                            &uuid::Uuid::new_v4().to_string(),
                            CHILD,
                            "Revise somente a validação de entradas.",
                            redirect,
                        )
                        .unwrap();
                    sent = true;
                }
            }
        })
        .unwrap();
        inbox.finish("Sem confirmação no encerramento.");
        (
            outcome,
            inbox.snapshot.receipts,
            observed,
            started.elapsed(),
        )
    }

    #[test]
    #[cfg(unix)]
    fn fake_server_waits_for_explicit_approval_and_receives_both_decisions() {
        for mode in [
            "approval_accept",
            "approval_decline",
            "approval_mcp_accept",
            "approval_mcp_decline",
        ] {
            let (outcome, _, events, elapsed) = run_fixture(mode, false, 100);
            assert!(outcome.success(), "{outcome:?}");
            assert!(elapsed >= Duration::from_millis(200));
            assert!(
                events
                    .iter()
                    .any(|line| line.contains("fixture/approvalReceived"))
            );
        }
    }

    #[test]
    fn approvals_require_a_decision_and_do_not_survive_resolution_or_shutdown() {
        let mailbox = Mailbox::new().unwrap();
        let mut pending =
            PendingApprovals::new(mailbox.path().join("approvals"), *mailbox.approval_key());
        let known = HashSet::from([ROOT.to_string()]);
        let request = json!({"id":"opaque/rpc-id","method":"item/commandExecution/requestApproval","params":{"threadId":ROOT,"turnId":"turn","command":"git add file"}});
        assert!(pending.enqueue(&request, &known, &HashMap::new()).unwrap());
        let (tx, rx) = mpsc::sync_channel(10);
        let writer = RpcWriter(tx);
        pending.poll(&writer).unwrap();
        assert!(rx.try_recv().is_err(), "No response before user decision");
        let approval = mailbox.approvals().remove(0);
        mailbox.decide_approval(&approval, true).unwrap();
        assert!(mailbox.decide_approval(&approval, false).is_err());
        pending.poll(&writer).unwrap();
        let reply: Value = serde_json::from_slice(&rx.recv().unwrap()).unwrap();
        assert_eq!(
            reply,
            json!({"id":"opaque/rpc-id","result":{"decision":"accept"}})
        );
        assert!(mailbox.approvals().is_empty());
        assert!(mailbox.decide_approval(&approval, true).is_err());
        assert!(pending.enqueue(&request, &known, &HashMap::new()).unwrap());
        pending.resolve(&request["id"]);
        assert!(mailbox.approvals().is_empty());
        assert!(pending.enqueue(&request, &known, &HashMap::new()).unwrap());
        drop(pending);
        assert!(mailbox.approvals().is_empty());
        let mut unknown = request.clone();
        unknown["params"]["threadId"] = json!("foreign");
        let mut pending =
            PendingApprovals::new(mailbox.path().join("approvals"), *mailbox.approval_key());
        assert!(!pending.enqueue(&unknown, &known, &HashMap::new()).unwrap());
        unknown["params"]["threadId"] = json!(ROOT);
        unknown["method"] = json!("item/tool/requestUserInput");
        assert!(!pending.enqueue(&unknown, &known, &HashMap::new()).unwrap());
        let invalid = ApprovalRequest {
            request_id: "../escape".into(),
            ..approval
        };
        assert!(mailbox.decide_approval(&invalid, true).is_err());
    }

    #[test]
    fn forged_decisions_and_replaced_displayed_commands_cannot_authorize_execution() {
        let mailbox = Mailbox::new().unwrap();
        let mut pending =
            PendingApprovals::new(mailbox.path().join("approvals"), *mailbox.approval_key());
        let request = json!({"id":42,"method":"item/commandExecution/requestApproval","params":{"threadId":ROOT,"command":"git add sensitive.txt"}});
        pending
            .enqueue(&request, &HashSet::from([ROOT.into()]), &HashMap::new())
            .unwrap();
        let original = mailbox.approvals().remove(0);
        let path = mailbox
            .path()
            .join("approvals")
            .join(format!("{}.decision", original.request_id));
        let (tx, rx) = mpsc::sync_channel(10);
        let writer = RpcWriter(tx);
        fs::write(&path, "true").unwrap();
        pending.poll(&writer).unwrap();
        assert!(rx.try_recv().is_err());
        let mut displayed = original.clone();
        displayed.params["command"] = json!("echo harmless");
        assert!(
            mailbox.decide_approval(&displayed, true).is_err(),
            "Do not sign a replaced snapshot"
        );
        fs::write(
            &path,
            serde_json::to_vec(&ApprovalDecision::signed(
                mailbox.approval_key(),
                &displayed,
                true,
            ))
            .unwrap(),
        )
        .unwrap();
        pending.poll(&writer).unwrap();
        assert!(rx.try_recv().is_err(), "MAC must bind original command");
        let mut decision = ApprovalDecision::signed(mailbox.approval_key(), &original, false);
        decision.accept = true;
        fs::write(&path, serde_json::to_vec(&decision).unwrap()).unwrap();
        pending.poll(&writer).unwrap();
        assert!(rx.try_recv().is_err(), "Cannot flip a signed denial");
        mailbox.decide_approval(&original, false).unwrap();
        pending.poll(&writer).unwrap();
        let result: Value = serde_json::from_slice(&rx.recv().unwrap()).unwrap();
        assert_eq!(result["result"]["decision"], "decline");
    }

    #[test]
    fn file_approval_requires_changes_from_the_same_thread() {
        let mailbox = Mailbox::new().unwrap();
        let mut pending =
            PendingApprovals::new(mailbox.path().join("approvals"), *mailbox.approval_key());
        let request = json!({"id":42,"method":"item/fileChange/requestApproval","params":{"threadId":ROOT,"itemId":"patch"}});
        let known = HashSet::from([ROOT.into()]);
        assert!(!pending.enqueue(&request, &known, &HashMap::new()).unwrap());
        let item = json!({"type":"fileChange","changes":[{"path":"file.txt","diff":"+hello"}]});
        let mut items = HashMap::from([(format!("{}:patch", json!("foreign")), item.clone())]);
        assert!(!pending.enqueue(&request, &known, &items).unwrap());
        items.insert(format!("{}:patch", json!(ROOT)), item.clone());
        assert!(pending.enqueue(&request, &known, &items).unwrap());
        assert_eq!(mailbox.approvals()[0].params["itemDetails"], item);
    }

    #[test]
    fn mcp_approval_does_not_answer_forms_or_foreign_threads() {
        let mailbox = Mailbox::new().unwrap();
        let mut pending =
            PendingApprovals::new(mailbox.path().join("approvals"), *mailbox.approval_key());
        let known = HashSet::from([ROOT.into()]);
        let request = json!({"id":"mcp", "method":"mcpServer/elicitation/request",
            "params":{"threadId":ROOT, "mode":"form", "serverName":"tools",
                "message":"Allow tool?", "requestedSchema":{"type":"object","properties":{}}}});
        for (field, value) in [
            ("threadId", json!("foreign")),
            ("mode", json!("url")),
            (
                "requestedSchema",
                json!({"type":"object","properties":{"name":{"type":"string"}}}),
            ),
            (
                "requestedSchema",
                json!({"type":"object","properties":{},"required":["name"]}),
            ),
            ("requestedSchema", Value::Null),
        ] {
            let mut invalid = request.clone();
            invalid["params"][field] = value;
            assert!(!pending.enqueue(&invalid, &known, &HashMap::new()).unwrap());
            assert!(mailbox.approvals().is_empty());
        }
        assert!(pending.enqueue(&request, &known, &HashMap::new()).unwrap());
        let approval = mailbox.approvals().remove(0);
        assert_eq!(approval.params, request["params"]);
        assert_eq!(
            PendingApprovals::response(&approval, true),
            json!({"action":"accept","content":{}})
        );
        assert_eq!(
            PendingApprovals::response(&approval, false),
            json!({"action":"decline","content":null})
        );
        pending.resolve(&json!("mcp"));
        assert!(mailbox.approvals().is_empty());
        assert!(mailbox.decide_approval(&approval, true).is_err());
    }

    #[test]
    fn permissions_are_limited_to_requested_fields_and_current_turn() {
        let request = ApprovalRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            method: "item/permissions/requestApproval".into(),
            params: json!({"permissions":{"network":{"enabled":true},"fileSystem":null,"unrecognized":true}}),
        };
        assert_eq!(
            PendingApprovals::response(&request, true),
            json!({"permissions":{"network":{"enabled":true}},"scope":"turn"})
        );
        assert_eq!(
            PendingApprovals::response(&request, false),
            json!({"permissions":{},"scope":"turn"})
        );
        let request = ApprovalRequest {
            method: "item/fileChange/requestApproval".into(),
            ..request
        };
        assert_eq!(
            PendingApprovals::response(&request, false),
            json!({"decision":"decline"})
        );
    }

    #[test]
    #[cfg(unix)]
    fn fake_server_confirms_send_and_redirect_without_provider_calls() {
        for redirect in [false, true] {
            let (outcome, receipts, observed, _) = run_fixture("success", redirect, 100);
            assert!(outcome.success(), "{outcome:?}");
            assert_eq!(outcome.reported_tokens.unwrap().total(), 140);
            assert!(
                receipts.iter().any(|r| r.status
                    == if redirect {
                        ControlStatus::Redirected
                    } else {
                        ControlStatus::Delivered
                    }),
                "{receipts:?}"
            );
            assert!(
                !observed
                    .iter()
                    .any(|s| s.contains("PRIVATE_FIXTURE_REASONING")
                        || s.contains("rawResponseItem"))
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn fake_server_rejection_and_missing_ack_never_report_delivery() {
        for mode in ["reject", "unconfirmed", "malformed_ack"] {
            let (_, receipts, _, _) = run_fixture(mode, false, 100);
            assert!(
                receipts.iter().any(|r| matches!(
                    r.status,
                    ControlStatus::Failed | ControlStatus::Unconfirmed
                )),
                "{mode}: {receipts:?}"
            );
            assert!(!receipts.iter().any(|r| matches!(
                r.status,
                ControlStatus::Delivered | ControlStatus::Redirected
            )));
        }
    }

    #[test]
    #[cfg(unix)]
    fn blocked_stdin_does_not_disable_timeout() {
        let (outcome, _, _, elapsed) = run_fixture("blocked_stdin", false, 1_000_000);
        assert!(outcome.timed_out);
        assert!(elapsed < Duration::from_secs(4), "{elapsed:?}");
    }
}
