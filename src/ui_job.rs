//! Background CLI execution owned by the terminal UI.
use anyhow::{Context, Result};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};
use tempfile::NamedTempFile;

const TAIL_BYTES: u64 = 64 * 1024;
const TAIL_LINES: usize = 256;
// A progress callback may wait up to 5 s for SQLite before the runner can act
// on SIGINT and clean up its separate provider group (up to another 300 ms).
const CANCEL_GRACE: Duration = Duration::from_secs(7);

pub(crate) struct JobResult {
    pub success: bool,
    pub cancelled: bool,
    pub message: String,
}

pub(crate) struct Job {
    child: Option<Child>,
    stdout: NamedTempFile,
    stderr: NamedTempFile,
    activity: Option<NamedTempFile>,
    control: Option<crate::agent_control::Mailbox>,
    export_path: Option<PathBuf>,
    started: Instant,
    finished: Option<Instant>,
    cancel_started: Option<Instant>,
    force_stopped: bool,
}

impl Job {
    pub(crate) fn start(
        executable: &Path,
        args: &[OsString],
        cwd: &Path,
        export_path: Option<PathBuf>,
    ) -> Result<Self> {
        Self::start_inner(executable, args, cwd, export_path, false)
    }

    pub(crate) fn start_tracked(executable: &Path, args: &[OsString], cwd: &Path) -> Result<Self> {
        Self::start_inner(executable, args, cwd, None, true)
    }

    fn start_inner(
        executable: &Path,
        args: &[OsString],
        cwd: &Path,
        export_path: Option<PathBuf>,
        track: bool,
    ) -> Result<Self> {
        let stdout =
            NamedTempFile::new().context("Não foi possível preparar a saída do comando")?;
        let stderr =
            NamedTempFile::new().context("Não foi possível preparar o registro de erros")?;
        let activity = track
            .then(NamedTempFile::new)
            .transpose()
            .context("Não foi possível preparar o painel de subagentes")?;
        let control = track
            .then(crate::agent_control::Mailbox::new)
            .transpose()
            .context("Não foi possível preparar o canal dos subagentes")?;
        let mut command = Command::new(executable);
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(stdout.reopen()?)
            .stderr(stderr.reopen()?);
        // This file belongs to one UI job. Native providers never inherit its path.
        if let Some(file) = &activity {
            command.env(crate::activity::ENV_PATH, file.path());
        } else {
            command.env_remove(crate::activity::ENV_PATH);
        }
        if let Some(mailbox) = &control {
            command.env(crate::agent_control::ENV_PATH, mailbox.path());
        } else {
            command.env_remove(crate::agent_control::ENV_PATH);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Redirecting stdio and creating a process group still leaves
            // /dev/tty attached to the UI. A provider's terminal cleanup can
            // then disable its mouse reporting or change its input modes.
            // A new session detaches that controlling terminal while retaining
            // an owned process group for cancellation of the job and helpers.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let child = command
            .spawn()
            .with_context(|| format!("Não foi possível iniciar {}", executable.display()))?;
        Ok(Self {
            child: Some(child),
            stdout,
            stderr,
            activity,
            control,
            export_path: export_path.map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                }
            }),
            started: Instant::now(),
            finished: None,
            cancel_started: None,
            force_stopped: false,
        })
    }

    pub(crate) fn poll(&mut self) -> Result<Option<JobResult>> {
        if self.child.is_none() {
            return Ok(None);
        }
        if self
            .cancel_started
            .is_some_and(|started| started.elapsed() >= CANCEL_GRACE)
            && !self.force_stopped
        {
            self.force_stop();
        }
        let status = self
            .child
            .as_mut()
            .expect("active child checked above")
            .try_wait()
            .context("Não foi possível consultar o comando em execução")?;
        let Some(status) = status else {
            return Ok(None);
        };
        self.child.take(); // try_wait has reaped the completed process.
        self.finished = Some(Instant::now());
        Ok(Some(self.result(status)))
    }

    fn result(&self, status: ExitStatus) -> JobResult {
        let cancelled = self.cancelling();
        if cancelled {
            return JobResult {
                success: false,
                cancelled: true,
                message: "Comando cancelado.".into(),
            };
        }
        if !status.success() {
            return JobResult {
                success: false,
                cancelled: false,
                message: status.code().map_or_else(
                    || "O comando foi interrompido por um sinal. Consulte a saída abaixo.".into(),
                    |code| format!("O comando falhou (código {code}). Consulte a saída abaixo."),
                ),
            };
        }
        let message = match &self.export_path {
            Some(path) => match self.export(path) {
                Ok(()) => format!("Concluído. Exportação salva em {}.", path.display()),
                Err(error) => {
                    return JobResult {
                        success: false,
                        cancelled: false,
                        message: format!(
                            "Comando concluído, mas não foi possível salvar a exportação: {error:#}"
                        ),
                    };
                }
            },
            None => "Comando concluído.".into(),
        };
        JobResult {
            success: true,
            cancelled: false,
            message,
        }
    }

    fn export(&self, path: &Path) -> Result<()> {
        let mut source = self.stdout.reopen()?;
        let mut target = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("{} (arquivos existentes são preservados)", path.display()))?;
        let copied = std::io::copy(&mut source, &mut target)
            .and_then(|_| target.sync_all())
            .with_context(|| format!("Falha ao gravar {}", path.display()));
        if copied.is_err() {
            // Only remove the new file owned by this export, never an existing destination.
            drop(target);
            let _ = fs::remove_file(path);
        }
        copied
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        let mut lines = tail(self.stdout.path());
        lines.extend(
            tail(self.stderr.path())
                .into_iter()
                .map(|line| format!("stderr: {line}")),
        );
        lines
    }

    pub(crate) fn activity(&self) -> Option<crate::activity::ActivitySnapshot> {
        let file = File::open(self.activity.as_ref()?.path()).ok()?;
        const LIMIT: u64 = 512 * 1024;
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > LIMIT {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    }

    pub(crate) fn cancel(&mut self) {
        if self.child.is_none() || self.cancel_started.is_some() {
            return;
        }
        self.cancel_started = Some(Instant::now());
        if let Some(child) = &mut self.child {
            #[cfg(unix)]
            unsafe {
                // This group belongs exclusively to the spawned CLI and its helpers.
                libc::kill(-(child.id() as i32), libc::SIGINT);
            }
            #[cfg(not(unix))]
            let _ = child.kill();
        }
    }

    pub(crate) fn agent_capability(&self) -> crate::agent_control::ControlCapability {
        if self.child.is_none() || self.cancelling() {
            return crate::agent_control::ControlCapability::unavailable(
                "Esta execução está encerrada ou sendo cancelada.",
            );
        }
        self.control
            .as_ref()
            .map(|m| m.capability())
            .unwrap_or_else(|| {
                crate::agent_control::ControlCapability::unavailable(
                    "Este comando não possui canal de controle de subagentes.",
                )
            })
    }

    pub(crate) fn send_agent_control(
        &self,
        request_id: &str,
        agent_id: &str,
        message: &str,
        redirect: bool,
    ) -> Result<()> {
        anyhow::ensure!(
            self.child.is_some() && !self.cancelling(),
            "Esta execução está encerrada ou sendo cancelada."
        );
        self.control
            .as_ref()
            .context("Este comando não possui canal de controle de subagentes")?
            .submit(request_id, agent_id, message, redirect)
    }

    pub(crate) fn control_receipts(&self) -> Vec<crate::agent_control::ControlReceipt> {
        self.control
            .as_ref()
            .map(|m| m.receipts())
            .unwrap_or_default()
    }

    fn force_stop(&mut self) {
        if let Some(child) = &mut self.child {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
        }
        self.force_stopped = true;
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.finished
            .unwrap_or_else(Instant::now)
            .duration_since(self.started)
    }

    pub(crate) fn cancelling(&self) -> bool {
        self.cancel_started.is_some()
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let Some(child) = &mut self.child else {
            return;
        };
        if child.try_wait().ok().flatten().is_some() {
            self.child.take();
            return;
        }
        self.cancel();
        let until = self.cancel_started.unwrap_or_else(Instant::now) + CANCEL_GRACE;
        while Instant::now() < until {
            if self
                .child
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten())
                .is_some()
            {
                self.child.take();
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.force_stop();
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

fn tail(path: &Path) -> Vec<String> {
    let result = (|| -> std::io::Result<Vec<String>> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let offset = length.saturating_sub(TAIL_BYTES);
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(TAIL_BYTES).read_to_end(&mut bytes)?;
        let text = String::from_utf8_lossy(&bytes);
        let mut lines: Vec<_> = text.lines().map(sanitize).collect();
        if offset > 0 && lines.len() > 1 {
            lines.remove(0); // The first line may begin in the middle of a UTF-8 character.
        }
        let omitted = offset > 0 || lines.len() > TAIL_LINES;
        if lines.len() > TAIL_LINES {
            lines.drain(..lines.len() - TAIL_LINES);
        }
        if omitted {
            lines.insert(0, "… saída anterior omitida".into());
        }
        Ok(lines)
    })();
    result.unwrap_or_else(|error| vec![format!("Não foi possível ler a saída: {error}")])
}

/// Strip terminal escape sequences before rendering provider or command output.
fn sanitize(text: &str) -> String {
    let mut chars = text.chars().peekable();
    let mut output = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\t' => output.push(' '),
            c if !c.is_control() => output.push(c),
            _ => {}
        }
    }
    output
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fixture(script: &str, extra: &[&str], directory: &Path, export: Option<PathBuf>) -> Job {
        let mut args: Vec<OsString> = ["-c", script, "fixture"]
            .into_iter()
            .map(OsString::from)
            .collect();
        args.extend(extra.iter().map(OsString::from));
        Job::start(Path::new("/bin/sh"), &args, directory, export).unwrap()
    }

    fn finish(job: &mut Job) -> JobResult {
        let deadline = Instant::now() + CANCEL_GRACE + Duration::from_secs(2);
        loop {
            if let Some(result) = job.poll().unwrap() {
                return result;
            }
            assert!(Instant::now() < deadline, "fixture failed to complete");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn background_job_has_no_ui_terminal_and_keeps_its_own_cancellation_group() {
        let directory = tempdir().unwrap();
        let mut job = fixture(
            "if (: </dev/tty) 2>/dev/null; then printf shared; else printf detached; fi; exec sleep 30",
            &[],
            directory.path(),
            None,
        );
        let pid = job.child.as_ref().unwrap().id() as libc::pid_t;
        let deadline = Instant::now() + Duration::from_secs(2);
        while job.lines().is_empty() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(unsafe { libc::getsid(pid) }, pid);
        assert_ne!(unsafe { libc::getsid(pid) }, unsafe { libc::getsid(0) });
        assert_eq!(unsafe { libc::getpgid(pid) }, pid);
        assert_eq!(job.lines(), ["detached"]);
        job.cancel();
        assert!(finish(&mut job).cancelled);
    }

    #[test]
    fn arguments_are_literal_and_only_stdout_is_exported_once() {
        let directory = tempdir().unwrap();
        let argument = "$(touch injected); `echo unsafe` with spaces";
        let mut job = fixture(
            "printf '%s\\n' \"$1\"; printf 'diagnostic\\n' >&2",
            &[argument],
            directory.path(),
            Some("result.txt".into()),
        );
        let result = finish(&mut job);
        assert!(result.success && !result.cancelled);
        assert_eq!(
            fs::read_to_string(directory.path().join("result.txt")).unwrap(),
            format!("{argument}\n")
        );
        assert!(!directory.path().join("injected").exists());
        assert!(job.lines().iter().any(|line| line == "stderr: diagnostic"));
        assert!(job.poll().unwrap().is_none());
        let elapsed = job.elapsed();
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(job.elapsed(), elapsed);
    }

    #[test]
    fn activity_is_private_scoped_to_one_job_and_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempdir().unwrap();
        let snapshot = serde_json::json!({"agents": [{
            "id":"child", "parent_id":"root", "title":"Inspecionar o projeto",
            "model":"observed-model", "effort":"high", "preview":"Lendo src/main.rs",
            "status":"running"
        }], "supported":true, "omitted":0})
        .to_string();
        let args = vec![
            OsString::from("-c"),
            OsString::from("printf '%s' \"$1\" > \"$STACKPULSE_ACTIVITY_FILE\""),
            OsString::from("fixture"),
            OsString::from(snapshot),
        ];
        let mut job = Job::start_tracked(Path::new("/bin/sh"), &args, directory.path()).unwrap();
        let path = job.activity.as_ref().unwrap().path().to_owned();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(finish(&mut job).success);
        let activity = job.activity().expect("snapshot from this job");
        assert_eq!(activity.agents.len(), 1);
        assert_eq!(activity.agents[0].model.as_deref(), Some("observed-model"));
        assert!(
            job.lines().is_empty(),
            "activity must not enter the answer transcript"
        );
        let other = fixture("true", &[], directory.path(), None);
        assert!(other.activity().is_none());
        fs::write(&path, b"{partial").unwrap();
        assert!(job.activity().is_none());
        fs::write(&path, vec![b' '; 512 * 1024 + 1]).unwrap();
        assert!(job.activity().is_none());
        drop(job);
        assert!(!path.exists());
    }

    #[test]
    fn export_never_overwrites_existing_files_and_failed_commands_do_not_export() {
        let directory = tempdir().unwrap();
        let existing = directory.path().join("existing.json");
        fs::write(&existing, "original").unwrap();
        let mut job = fixture(
            "printf changed",
            &[],
            directory.path(),
            Some(existing.clone()),
        );
        let result = finish(&mut job);
        assert!(!result.success && !result.cancelled);
        assert!(result.message.contains("exportação"));
        assert_eq!(fs::read_to_string(existing).unwrap(), "original");
        let failed = directory.path().join("failed.json");
        let mut job = fixture(
            "printf partial; exit 7",
            &[],
            directory.path(),
            Some(failed.clone()),
        );
        let result = finish(&mut job);
        assert!(!result.success);
        assert!(result.message.contains('7'));
        assert!(!failed.exists());
    }

    #[test]
    fn cancelling_allows_the_cli_to_clean_up_and_never_exports_partial_output() {
        let directory = tempdir().unwrap();
        let export = directory.path().join("cancelled.txt");
        let mut job = fixture(
            "trap 'printf cleaned > cleaned; exit 130' INT; printf ready; while :; do sleep 0.1; done",
            &[],
            directory.path(),
            Some(export.clone()),
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while job.lines().is_empty() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        job.cancel();
        job.cancel();
        assert!(job.cancelling());
        let result = finish(&mut job);
        assert!(result.cancelled && !result.success);
        assert!(directory.path().join("cleaned").exists());
        assert!(!export.exists());
    }

    #[test]
    fn drop_interrupts_and_reaps_its_process() {
        let directory = tempdir().unwrap();
        let job = fixture(
            "trap 'printf cleaned > dropped; exit 130' INT; printf ready; while :; do sleep 0.1; done",
            &[],
            directory.path(),
            None,
        );
        let pid = job.child.as_ref().unwrap().id();
        let deadline = Instant::now() + Duration::from_secs(2);
        while job.lines().is_empty() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(job);
        assert!(directory.path().join("dropped").exists());
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    }

    #[test]
    fn cancellation_allows_a_busy_callback_to_finish_before_cleanup() {
        let directory = tempdir().unwrap();
        let mut job = fixture(
            "trap 'sleep 5; printf cleaned > delayed; exit 130' INT; printf ready; while :; do sleep 0.1; done",
            &[],
            directory.path(),
            None,
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while job.lines().is_empty() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        job.cancel();
        let result = finish(&mut job);
        assert!(result.cancelled);
        assert!(directory.path().join("delayed").exists());
        assert!(!job.force_stopped);
    }

    #[test]
    fn output_is_bounded_and_escape_sequences_cannot_control_the_terminal() {
        let file = NamedTempFile::new().unwrap();
        fs::write(
            file.path(),
            "\u{1b}[31mred\u{1b}[0m\n\u{1b}]0;secret title\u{7}safe\tline\n",
        )
        .unwrap();
        assert_eq!(tail(file.path()), ["red", "safe line"]);
        fs::write(file.path(), "界\n".repeat(100_000)).unwrap();
        let output = tail(file.path());
        assert!(output.len() <= TAIL_LINES + 1);
        assert!(output[0].contains("omitida"));
        assert!(
            output
                .iter()
                .all(|line| !line.chars().any(char::is_control))
        );
    }
}
