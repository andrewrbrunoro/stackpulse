//! Per-run MCP bridge. The root chooses tasks; this server fixes their provider,
//! model, effort, workspace and permission mode from the reviewed profile.
use crate::{
    client::Backend,
    profiles::{Settings, TeamSpec},
    runner::{self, Outcome, Request},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Serialize, Deserialize)]
struct Config {
    settings: Settings,
    team: TeamSpec,
    cwd: PathBuf,
    sandbox: String,
    timeout_secs: u64,
    executables: HashMap<String, PathBuf>,
}

pub struct Bridge {
    directory: tempfile::TempDir,
}
impl Bridge {
    pub fn prepare(request: &Request<'_>, command: &mut Command) -> Result<Self> {
        ensure!(
            cfg!(unix),
            "A ponte multi-provider requer Unix para encerrar os grupos de processos dos subagentes"
        );
        let team = request.team.context("Equipe ausente")?;
        team.validate_execution_providers()?;
        ensure!(
            Backend::for_provider(team.root_provider()) == Some(request.settings.client),
            "CLI root não corresponde ao provider do perfil"
        );
        ensure!(
            request.delegates,
            "Equipe mista requer delegação habilitada"
        );
        ensure!(
            matches!(request.settings.client, Backend::Codex | Backend::Claude),
            "Equipe mista requer root Codex ou Claude"
        );
        ensure!(
            request.image.is_none() && request.output_schema.is_none(),
            "Ponte multi-provider não é usada para compilação de perfis"
        );
        let directory = tempfile::Builder::new()
            .prefix("stackpulse-bridge-")
            .tempdir()?;
        let dirs = crate::cli_providers::search_directories();
        let mut executables = HashMap::new();
        for agent in &team.agents {
            let provider = team.provider_for(agent);
            let backend =
                Backend::for_provider(provider).context("Provider de subagente não suportado")?;
            let executable = if backend == request.settings.client {
                request.settings.executable.clone()
            } else {
                crate::cli_providers::resolve_executable(Path::new(backend.command()), &dirs)
                    .with_context(|| {
                        format!(
                            "Instale {} para executar o papel {} ({provider})",
                            backend.label(),
                            agent.role
                        )
                    })?
            };
            executables.insert(agent.role.clone(), executable);
        }
        let config = Config {
            settings: request.settings.clone(),
            team: team.clone(),
            cwd: request.cwd.canonicalize()?,
            sandbox: request.sandbox.into(),
            timeout_secs: request.timeout_secs,
            executables,
        };
        let path = directory.path().join("config.json");
        private_write(&path, &serde_json::to_vec(&config)?)?;
        let executable = std::env::current_exe()?;
        let args = vec![
            "mixed-bridge".to_string(),
            "--config".into(),
            path.to_string_lossy().into_owned(),
        ];
        match request.settings.client {
            Backend::Codex => {
                for (key, value) in [
                    (
                        "mcp_servers.stackpulse_team.command",
                        toml::Value::String(executable.to_string_lossy().into_owned()),
                    ),
                    (
                        "mcp_servers.stackpulse_team.args",
                        toml::Value::Array(args.iter().cloned().map(toml::Value::String).collect()),
                    ),
                    (
                        "mcp_servers.stackpulse_team.required",
                        toml::Value::Boolean(true),
                    ),
                    (
                        "mcp_servers.stackpulse_team.tool_timeout_sec",
                        toml::Value::Integer(40),
                    ),
                ] {
                    command.arg("-c").arg(format!("{key}={value}"));
                }
            }
            Backend::Claude => {
                command.arg("--mcp-config").arg(
                    json!({"mcpServers":{"stackpulse_team":{"command":executable,"args":args}}})
                        .to_string(),
                );
                // Only this profile-bound dispatcher is preapproved, not Bash or
                // arbitrary MCP servers. Each child keeps the root's sandbox.
                command.arg("--allowedTools").arg("mcp__stackpulse_team__spawn,mcp__stackpulse_team__status,mcp__stackpulse_team__wait,mcp__stackpulse_team__cancel");
            }
            _ => unreachable!(),
        }
        Ok(Self { directory })
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = fs::write(self.directory.path().join("cancel"), b"cancel");
        // Active groups are registered before reading any model output. This
        // also catches children whose MCP server was killed with its root.
        if let Ok(entries) = fs::read_dir(self.directory.path()) {
            for entry in entries.flatten() {
                if let Some(pid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| s.strip_prefix("pid-"))
                    .and_then(|s| s.rsplit('-').next())
                    .and_then(|s| s.parse::<i32>().ok())
                {
                    #[cfg(unix)]
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                }
            }
        }
    }
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)?;
    Ok(())
}

pub(crate) struct ChildControl {
    pub cancelled: Arc<AtomicBool>,
    pub directory: PathBuf,
    pub registration_id: String,
}
impl ChildControl {
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed) || self.directory.join("cancel").exists()
    }
    pub fn cleanup_unstarted(&self) {
        // Command::spawn reaps a child that failed before exec. Remove only this
        // task's marker; unrelated concurrent process groups remain registered.
        if let Ok(entries) = fs::read_dir(&self.directory) {
            let prefix = format!("pid-{}-", self.registration_id);
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(&prefix))
                {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }
    #[cfg(unix)]
    pub fn configure_child(&self, command: &mut Command) -> Result<()> {
        use std::{
            ffi::CString,
            os::unix::{ffi::OsStrExt, process::CommandExt},
        };
        let prefix = self
            .directory
            .join(format!("pid-{}-", self.registration_id))
            .as_os_str()
            .as_bytes()
            .to_vec();
        ensure!(prefix.len() + 20 < 4096, "Caminho temporário muito longo");
        let cancelled = CString::new(self.directory.join("cancel").as_os_str().as_bytes())?;
        let config = CString::new(self.directory.join("config.json").as_os_str().as_bytes())?;
        // Only async-signal-safe syscalls and stack operations after fork.
        // Initially the child belongs to its bridge's group. Register its future
        // group BEFORE separating it, then recheck cancellation. Consequently
        // root teardown either kills the inherited group, sees this registration,
        // or causes this child to abort before exec. There is no unregistered
        // independent process between spawn() and the parent's PID bookkeeping.
        unsafe {
            command.pre_exec(move || {
                let mut filename = [0u8; 4096];
                filename[..prefix.len()].copy_from_slice(&prefix);
                let mut digits = [0u8; 20];
                let mut pid = libc::getpid() as u32;
                let mut start = digits.len();
                loop {
                    start -= 1;
                    digits[start] = b'0' + (pid % 10) as u8;
                    pid /= 10;
                    if pid == 0 {
                        break;
                    }
                }
                let end = prefix.len() + digits.len() - start;
                filename[prefix.len()..end].copy_from_slice(&digits[start..]);
                let fd = libc::open(
                    filename.as_ptr().cast(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                    0o600,
                );
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(fd);
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::access(cancelled.as_ptr(), libc::F_OK) == 0
                    || libc::access(config.as_ptr(), libc::F_OK) != 0
                {
                    return Err(std::io::Error::from_raw_os_error(libc::ECANCELED));
                }
                Ok(())
            });
        }
        Ok(())
    }
    pub fn register(&self, pid: u32) -> Result<Registration> {
        let path = self
            .directory
            .join(format!("pid-{}-{pid}", self.registration_id));
        #[cfg(not(unix))]
        private_write(&path, b"active")?;
        Ok(Registration { path, pid })
    }
}
pub(crate) struct Registration {
    path: PathBuf,
    pid: u32,
}
impl Drop for Registration {
    fn drop(&mut self) {
        // A successful CLI can leave background helpers after its leader exits.
        // End the task's whole group before removing its recovery registration.
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.pid as i32), libc::SIGKILL);
        }
        let _ = fs::remove_file(&self.path);
    }
}

struct Job {
    role: String,
    cancelled: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Result<Outcome, String>>,
    result: Option<Result<Outcome, String>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Job {
    fn refresh(&mut self) {
        if self.result.is_none() {
            match self.receiver.try_recv() {
                Ok(result) => self.result = Some(result),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.result = Some(Err("Worker encerrou sem resultado".into()))
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }
    fn value(&mut self, id: &str) -> Value {
        self.refresh();
        match &self.result {
            None => json!({"id":id,"role":self.role,"status":"running"}),
            Some(Ok(outcome)) => {
                json!({"id":id,"role":self.role,"status":if outcome.success(){"completed"}else{"failed"},"outcome":outcome})
            }
            Some(Err(error)) => json!({"id":id,"role":self.role,"status":"failed","error":error}),
        }
    }
}
struct Server {
    config: Arc<Config>,
    directory: PathBuf,
    jobs: HashMap<String, Job>,
    shutdown: Arc<AtomicBool>,
    deadline: Instant,
}
impl Server {
    fn stopping(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
            || self.directory.join("cancel").exists()
            || Instant::now() >= self.deadline
    }
    fn call(&mut self, name: &str, args: &Value) -> Result<Value> {
        ensure!(!self.stopping(), "Execução cancelada ou prazo esgotado");
        if name == "spawn" {
            // Explicit tasks only; provider/executable/permissions cannot be
            // supplied by the model through tool arguments.
            let role = args["role"].as_str().context("role obrigatório")?;
            let prompt = args["prompt"].as_str().context("prompt obrigatório")?;
            ensure!(
                !prompt.trim().is_empty() && prompt.len() <= 1_000_000,
                "Tarefa vazia ou maior que 1 MB"
            );
            let agent = self
                .config
                .team
                .agents
                .iter()
                .find(|a| a.role == role)
                .context("Papel não pertence à equipe")?
                .clone();
            for job in self.jobs.values_mut() {
                job.refresh();
            }
            let limit = if self.config.team.delegation == "sequential" {
                1
            } else {
                self.config.settings.max_agents as usize
            };
            ensure!(
                self.jobs.values().filter(|j| j.result.is_none()).count() < limit,
                "Limite de subagentes ativos atingido; aguarde um resultado"
            );
            ensure!(
                self.jobs.len() < 1000,
                "Limite de 1000 tarefas por execução atingido"
            );
            let id = uuid::Uuid::new_v4().to_string();
            let cancelled = Arc::new(AtomicBool::new(false));
            let control = ChildControl {
                cancelled: cancelled.clone(),
                directory: self.directory.clone(),
                registration_id: id.clone(),
            };
            let config = self.config.clone();
            let task = format!(
                "{}\n\nTAREFA DELEGADA:\n{}",
                crate::team_runtime::child_instructions(&config.team, &agent),
                prompt
            );
            let remaining = self
                .deadline
                .saturating_duration_since(Instant::now())
                .as_secs()
                .max(1);
            let timeout = args
                .get("timeout_secs")
                .map(|v| {
                    v.as_u64()
                        .filter(|n| *n > 0)
                        .context("timeout_secs deve ser inteiro positivo")
                })
                .transpose()?
                .unwrap_or(remaining)
                .min(remaining);
            let (tx, receiver) = mpsc::channel();
            let thread = thread::spawn(move || {
                let provider = Backend::for_provider(config.team.provider_for(&agent))
                    .expect("validated provider")
                    .default_provider();
                let mut settings = config.settings.clone();
                settings.client = Backend::for_provider(provider).expect("validated provider");
                settings.executable = config.executables[&agent.role].clone();
                settings.provider = provider.into();
                settings.model = agent.model.clone();
                settings.effort = agent.effort.clone();
                let request = Request {
                    settings: &settings,
                    cwd: &config.cwd,
                    model: &agent.model,
                    effort: &agent.effort,
                    provider,
                    prompt: &task,
                    image: None,
                    output_schema: None,
                    sandbox: &config.sandbox,
                    timeout_secs: timeout,
                    delegates: false,
                    team: None,
                };
                let result = runner::execute_controlled(request, |_| Ok(()), |_| {}, Some(&control)).map(|mut outcome| {
                    if outcome.final_message.len() > 1_000_000 {
                        let mut end = 1_000_000;
                        while !outcome.final_message.is_char_boundary(end) { end -= 1; }
                        outcome.final_message.truncate(end);
                        outcome.failed = true;
                        outcome.error_message = Some("Resposta do subagente excede 1 MB; texto truncado, solicite entrega menor".into());
                    }
                    outcome
                }).map_err(|e|format!("{e:#}"));
                let _ = tx.send(result);
            });
            self.jobs.insert(
                id.clone(),
                Job {
                    role: role.into(),
                    cancelled,
                    receiver,
                    result: None,
                    thread: Some(thread),
                },
            );
            return Ok(self.jobs.get_mut(&id).unwrap().value(&id));
        }
        ensure!(
            matches!(name, "status" | "wait" | "cancel"),
            "Ferramenta desconhecida"
        );
        let id = args["id"].as_str().context("id obrigatório")?;
        ensure!(self.jobs.contains_key(id), "Subagente desconhecido");
        if name == "cancel" {
            self.jobs
                .get_mut(id)
                .unwrap()
                .cancelled
                .store(true, Ordering::Relaxed);
        }
        if name == "wait" {
            let seconds = args
                .get("timeout_secs")
                .map(|v| v.as_u64().context("timeout_secs deve ser inteiro positivo"))
                .transpose()?
                .unwrap_or(20)
                .min(30);
            let until = Instant::now() + Duration::from_secs(seconds);
            while !self.stopping() && Instant::now() < until {
                let job = self.jobs.get_mut(id).unwrap();
                job.refresh();
                if job.result.is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
        Ok(self.jobs.get_mut(id).unwrap().value(id))
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        for job in self.jobs.values() {
            job.cancelled.store(true, Ordering::Relaxed);
        }
        for job in self.jobs.values_mut() {
            if let Some(handle) = job.thread.take() {
                let _ = handle.join();
            }
        }
    }
}

fn tools() -> Value {
    let mut list = vec![
        json!({"name":"spawn","description":"Start a task with a configured team role. Its provider/model/effort/permissions are fixed by the profile. Share complete task context; wait for results before final delivery.","inputSchema":{"type":"object","properties":{"role":{"type":"string"},"prompt":{"type":"string"},"timeout_secs":{"type":"integer","minimum":1}},"required":["role","prompt"],"additionalProperties":false}}),
    ];
    for name in ["status", "wait", "cancel"] {
        let mut properties = json!({"id":{"type":"string"}});
        if name == "wait" {
            properties["timeout_secs"] = json!({"type":"integer","minimum":0,"maximum":30});
        }
        list.push(json!({"name":name,"description":format!("{name} a delegated task; failed outcomes must be handled explicitly."),"inputSchema":{"type":"object","properties":properties,"required":["id"],"additionalProperties":false}}));
    }
    json!({"tools":list})
}

/// Hidden internal entry point; called only with a per-run private config.
pub fn serve(path: &Path) -> Result<()> {
    let config: Config = serde_json::from_slice(&fs::read(path)?)?;
    config.team.validate()?;
    config.team.validate_execution_providers()?;
    config.settings.validate()?;
    for agent in &config.team.agents {
        ensure!(
            config.executables.contains_key(&agent.role),
            "Executável ausente para papel {}",
            agent.role
        );
    }
    ensure!(
        matches!(
            config.sandbox.as_str(),
            "read-only" | "workspace-write" | "danger-full-access"
        ),
        "Sandbox inválido"
    );
    let shutdown = Arc::new(AtomicBool::new(false));
    let _int = runner::InterruptGuard(signal_hook::flag::register(
        signal_hook::consts::SIGINT,
        shutdown.clone(),
    )?);
    let _term = runner::InterruptGuard(signal_hook::flag::register(
        signal_hook::consts::SIGTERM,
        shutdown.clone(),
    )?);
    let eof = shutdown.clone();
    let (tx, rx) = mpsc::sync_channel(32);
    thread::spawn(move || {
        let mut reader = BufReader::new(std::io::stdin());
        loop {
            // Bound allocations for malformed or hostile clients.
            let mut line = Vec::new();
            match std::io::Read::take(&mut reader, 2_000_001).read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) if line.len() > 2_000_000 => break,
                Ok(_) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            }
        }
        eof.store(true, Ordering::Relaxed);
    });
    let deadline = Instant::now() + Duration::from_secs(config.timeout_secs);
    let mut server = Server {
        config: Arc::new(config),
        directory: path.parent().context("Config sem diretório")?.into(),
        jobs: HashMap::new(),
        shutdown,
        deadline,
    };
    let mut stdout = std::io::stdout().lock();
    while !server.stopping() {
        let line = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        let message: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let Some(id) = message.get("id") else {
            continue;
        };
        let result = match message["method"].as_str().unwrap_or("") {
            "initialize" => Ok(
                json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"stackpulse-team","version":"1.0"}}),
            ),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(tools()),
            "tools/call" => {
                let result = server.call(
                    message["params"]["name"].as_str().unwrap_or(""),
                    &message["params"]["arguments"],
                );
                let failed = result.is_err();
                let value = result.unwrap_or_else(|error| json!({"error":format!("{error:#}")}));
                Ok(json!({"content":[{"type":"text","text":value.to_string()}],"isError":failed}))
            }
            _ => Err(json!({"code":-32601,"message":"Method not found"})),
        };
        let response = match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => json!({"jsonrpc":"2.0","id":id,"error":error}),
        };
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

pub fn internal_entry() -> Option<Result<()>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_none_or(|a| a != "mixed-bridge") {
        return None;
    }
    Some((|| {
        ensure!(
            args.len() == 4 && args[2] == "--config",
            "Uso interno: mixed-bridge --config <arquivo>"
        );
        serve(Path::new(&args[3]))
    })())
}
