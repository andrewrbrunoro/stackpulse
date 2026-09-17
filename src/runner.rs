use crate::{
    client::Backend,
    model::Tokens,
    profiles::{Settings, TeamSpec},
    providers,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Outcome {
    pub thread_id: Option<String>,
    pub reported_tokens: Option<Tokens>,
    pub completed_turns: usize,
    pub failed: bool,
    pub final_message: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub interrupted: bool,
    pub invalid_events: usize,
    #[serde(default)]
    pub observed_model: Option<String>,
    #[serde(default)]
    pub observed_stack: Option<String>,
    #[serde(default)]
    pub reported_cost_usd: Option<f64>,
    #[serde(default)]
    pub reported_scope: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
}

impl Outcome {
    pub fn event(&mut self, line: &str) {
        self.event_for(Backend::Codex, line);
    }
    pub fn event_for(&mut self, client: Backend, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            self.invalid_events += 1;
            return;
        };
        match client {
            Backend::Claude => {
                providers::claude::event(self, &v);
                return;
            }
            Backend::Cursor | Backend::Grok => {
                providers::agent_cli::event(self, &v);
                return;
            }
            Backend::Codex => {}
        }
        match v["type"].as_str().unwrap_or("") {
            "thread.started" => self.thread_id = v["thread_id"].as_str().map(str::to_string),
            "turn.completed" => {
                self.completed_turns += 1;
                if v["usage"]["input_tokens"].is_u64()
                    && v["usage"]["output_tokens"].is_u64()
                    && let Ok(tokens) = serde_json::from_value::<Tokens>(v["usage"].clone())
                {
                    if tokens.valid() {
                        self.reported_tokens.get_or_insert_default().add(tokens);
                        self.reported_scope = Some("root_only".into());
                    } else {
                        self.invalid_events += 1;
                    }
                }
            }
            "turn.failed" => {
                self.failed = true;
                self.error_message = v["error"]["message"].as_str().map(str::to_string);
            }
            "item.completed" if v["item"]["type"] == "agent_message" => {
                if let Some(s) = v["item"]["text"].as_str() {
                    self.final_message = s.to_string();
                }
            }
            _ => {}
        }
    }
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
            && !self.failed
            && !self.timed_out
            && !self.interrupted
            && self.completed_turns > 0
    }
}

#[derive(Clone, Copy)]
pub struct Request<'a> {
    pub settings: &'a Settings,
    pub cwd: &'a Path,
    pub model: &'a str,
    pub effort: &'a str,
    pub provider: &'a str,
    pub prompt: &'a str,
    pub image: Option<&'a Path>,
    pub output_schema: Option<&'a Path>,
    pub sandbox: &'a str,
    pub timeout_secs: u64,
    pub delegates: bool,
    /// Selected execution profile; image extraction and standalone requests have none.
    pub team: Option<&'a TeamSpec>,
}

/// Owns per-run role configurations. Keep this guard alive until the CLI exits.
pub struct PreparedCommand {
    command: Command,
    _profile_files: Option<tempfile::TempDir>,
    _bridge: Option<crate::mixed_runtime::Bridge>,
}
impl std::ops::Deref for PreparedCommand {
    type Target = Command;
    fn deref(&self) -> &Self::Target {
        &self.command
    }
}
impl std::ops::DerefMut for PreparedCommand {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.command
    }
}

pub fn command(request: &Request<'_>) -> Result<PreparedCommand> {
    command_with_input(request, None, None)
}

fn command_with_input(
    request: &Request<'_>,
    input_file: Option<&Path>,
    control: Option<&crate::mixed_runtime::ChildControl>,
) -> Result<PreparedCommand> {
    let mut command = Command::new(&request.settings.executable);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        if let Some(control) = control {
            control.configure_child(&mut command)?;
        } else {
            command.process_group(0);
        }
    }
    command.current_dir(request.cwd);
    let mut profile_files = None;
    if let Some(team) = request.team {
        team.validate()?;
    }
    let mut native_request = *request;
    let mixed = request.team.is_some_and(|team| team.is_mixed());
    if mixed {
        native_request.team = None;
        native_request.delegates = false;
    }
    let original_request = request;
    let request = &native_request;
    match request.settings.client {
        Backend::Claude => providers::claude::configure(&mut command, request)?,
        Backend::Cursor | Backend::Grok => {
            providers::agent_cli::configure_with_input(&mut command, request, input_file)?
        }
        Backend::Codex => {
            profile_files = providers::codex::configure(&mut command, request)?;
        }
    }
    let bridge = if mixed {
        Some(crate::mixed_runtime::Bridge::prepare(
            original_request,
            &mut command,
        )?)
    } else {
        None
    };
    command
        .env_remove(crate::activity::ENV_PATH)
        .env_remove(crate::agent_control::ENV_PATH)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    Ok(PreparedCommand {
        command,
        _profile_files: profile_files,
        _bridge: bridge,
    })
}

pub(crate) struct ChildGuard(pub(crate) Child);
impl ChildGuard {
    pub(crate) fn stop(&mut self) {
        #[cfg(unix)]
        {
            // Isolated process group: terminate helpers as well as the CLI.
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGINT);
            }
            let until = Instant::now() + Duration::from_millis(300);
            while Instant::now() < until && self.0.try_wait().ok().flatten().is_none() {
                std::thread::sleep(Duration::from_millis(20));
            }
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            self.stop();
        }
    }
}
pub(crate) struct InterruptGuard(pub(crate) signal_hook::SigId);
impl Drop for InterruptGuard {
    fn drop(&mut self) {
        signal_hook::low_level::unregister(self.0);
    }
}

pub fn execute(
    request: Request<'_>,
    progress: impl FnMut(&Outcome) -> Result<()>,
) -> Result<Outcome> {
    execute_with_events(request, progress, |_| {})
}

/// Observers receive public CLI events and idle ticks without changing usage accounting.
pub fn execute_with_events(
    request: Request<'_>,
    progress: impl FnMut(&Outcome) -> Result<()>,
    events: impl FnMut(Option<&str>),
) -> Result<Outcome> {
    execute_controlled(request, progress, events, None)
}

pub(crate) fn execute_controlled(
    request: Request<'_>,
    mut progress: impl FnMut(&Outcome) -> Result<()>,
    mut events: impl FnMut(Option<&str>),
    control: Option<&crate::mixed_runtime::ChildControl>,
) -> Result<Outcome> {
    ensure!(request.prompt.len() <= 1_000_000, "Prompt maior que 1 MB");
    if request.settings.client == Backend::Codex
        && request.team.is_some_and(|team| !team.is_mixed())
        && request.image.is_none()
        && request.output_schema.is_none()
        && crate::agent_control::requested()
    {
        return crate::agent_control::execute(request, progress, events);
    }
    if request.team.is_some_and(|team| team.is_mixed())
        || (request.settings.client != Backend::Codex && request.team.is_some())
    {
        crate::agent_control::unsupported(request.settings.client);
    }
    let interrupted = Arc::new(AtomicBool::new(false));
    let _interrupt_guard = InterruptGuard(signal_hook::flag::register(
        signal_hook::consts::SIGINT,
        interrupted.clone(),
    )?);
    let prepared_input = match request.settings.client {
        Backend::Cursor | Backend::Grok => providers::agent_cli::prompt_file(&request)?,
        _ => None,
    };
    let prompt = if prepared_input.is_some() {
        String::new()
    } else {
        match request.settings.client {
            Backend::Codex => request.prompt.to_owned(),
            Backend::Claude => providers::claude::input(&request)?,
            Backend::Cursor | Backend::Grok => providers::agent_cli::input(&request)?,
        }
    };
    let mut prepared_command = command_with_input(
        &request,
        prepared_input.as_ref().map(|file| file.path()),
        control,
    )?;
    ensure!(
        !control.is_some_and(|c| c.cancelled()),
        "Subagente cancelado antes de iniciar"
    );
    let spawned = prepared_command.spawn().with_context(|| {
        format!(
            "Não foi possível iniciar {}",
            request.settings.executable.display()
        )
    });
    if spawned.is_err()
        && let Some(control) = control
    {
        control.cleanup_unstarted();
    }
    let mut child = ChildGuard(spawned?);
    let _registration = control.map(|c| c.register(child.0.id())).transpose()?;
    let mut stdin = child.0.stdin.take().context("stdin indisponível")?;
    let writer = std::thread::spawn(move || stdin.write_all(prompt.as_bytes()));
    let stdout = child.0.stdout.take().context("stdout indisponível")?;
    let (tx, rx) = mpsc::sync_channel(64);
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut outcome = Outcome::default();
    let deadline = Instant::now() + Duration::from_secs(request.timeout_secs);
    let mut next_notice = Instant::now() + Duration::from_secs(30);
    loop {
        if Instant::now() >= deadline
            || interrupted.load(Ordering::Relaxed)
            || control.is_some_and(|c| c.cancelled())
        {
            outcome.interrupted =
                interrupted.load(Ordering::Relaxed) || control.is_some_and(|c| c.cancelled());
            outcome.timed_out = !outcome.interrupted;
            child.stop();
            break;
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                let line = line?;
                if line.len() > 4_000_000 {
                    outcome.invalid_events += 1;
                    continue;
                }
                let prior = (outcome.thread_id.clone(), outcome.completed_turns);
                outcome.event_for(request.settings.client, &line);
                events(Some(&line));
                if prior != (outcome.thread_id.clone(), outcome.completed_turns) {
                    progress(&outcome)?;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // stdout can close before a process exits; continue enforcing the timeout.
                if let Some(status) = child.0.try_wait()? {
                    outcome.exit_code = status.code();
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        events(None);
        if Instant::now() >= next_notice {
            eprintln!("AI Timeline: execução em andamento...");
            next_notice = Instant::now() + Duration::from_secs(30);
        }
    }
    if outcome.exit_code.is_none() {
        outcome.exit_code = child.0.wait()?.code();
    }
    if writer.is_finished() {
        let _ = writer.join();
    }
    progress(&outcome)?;
    Ok(outcome)
}
