//! Optional installation of the official AI-Memory bundle, without agent setup.
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, IsTerminal, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub const PROJECT_URL: &str = "https://github.com/akitaonrails/ai-memory";
pub const CREATOR_URL: &str = "https://akitaonrails.com";
pub const VERSION: &str = "2.2.1";

pub struct Installation {
    pub executable: PathBuf,
    pub version: String,
    pub reused: bool,
}

pub fn default_prefix() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("HOME").context("HOME indisponível; informe --prefix.")?)
            .join(".local"),
    )
}

pub fn print_credits() {
    println!("AI-Memory · criado por Fabio Akita (AkitaOnRails)");
    println!("Projeto: {PROJECT_URL}\nCriador: {CREATOR_URL}");
}

pub fn print_installation(value: &Installation) {
    println!(
        "AI-Memory {} · {}",
        if value.reused {
            "já disponível"
        } else {
            "instalado"
        },
        value.version
    );
    println!("Executável: {}", value.executable.display());
    println!("Pacote disponível. Conexão aos agentes ainda não configurada pelo StackPulse.");
}

/// Interactive installers share the terminal UI; pipes and dumb terminals stay plain.
pub fn select_install(default: bool) -> Result<bool> {
    if crate::tui::available() {
        return crate::memory_ui::select(default);
    }
    println!("\nMEMÓRIA OPCIONAL");
    print_credits();
    println!("Instala o pacote oficial. Conexão aos agentes é uma etapa separada.");
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        return ask_install(default);
    }
    println!("[{}] Instalar AI-Memory", if default { "x" } else { " " });
    Ok(default)
}

pub fn ask_install(default: bool) -> Result<bool> {
    loop {
        print!(
            "Instalar AI-Memory? [{}]: ",
            if default { "S/n" } else { "s/N" }
        );
        io::stdout().flush()?;
        let mut value = String::new();
        ensure!(
            io::stdin().read_line(&mut value)? > 0,
            "Seleção encerrada sem resposta."
        );
        match value.trim().to_lowercase().as_str() {
            "" => return Ok(default),
            "s" | "sim" | "y" | "yes" => return Ok(true),
            "n" | "não" | "nao" | "no" => return Ok(false),
            _ => println!("Responda s ou n."),
        }
    }
}

fn release(os: &str, arch: &str) -> Result<(&'static str, &'static str)> {
    // Hashes published alongside v2.2.1; pin both version and content.
    match (os, arch) {
        ("macos", "aarch64") => Ok((
            "ai-memory-macos-aarch64.tar.gz",
            "68972c697887ab28edd70f97a6a6f5774ea63c2bdd1b4ba854ce4c436c11850e",
        )),
        ("macos", "x86_64") => Ok((
            "ai-memory-macos-x86_64.tar.gz",
            "4d4d0351941e1e5daae7609f3bc922704d9c20b24bfda787b5f6ad76be216755",
        )),
        ("linux", "aarch64") => Ok((
            "ai-memory-linux-aarch64.tar.gz",
            "21205fe7c2ffa6d34ca043fd222624b7ccd072ed28f88c9d7b21fc445552f433",
        )),
        ("linux", "x86_64") => Ok((
            "ai-memory-linux-x86_64.tar.gz",
            "98d5f70976d201823ff76fa2bc1853a9e89ce11ce130c0c30d0ff8b97056916b",
        )),
        _ => bail!(
            "AI-Memory: plataforma sem pacote compatível ({os}/{arch}). Consulte {PROJECT_URL}."
        ),
    }
}

const OUTPUT_LIMIT: u64 = 64 * 1024;

struct RunSignals {
    interrupted: Arc<AtomicBool>,
    registrations: Vec<signal_hook::SigId>,
}

impl RunSignals {
    fn register() -> Result<Self> {
        let mut value = Self {
            interrupted: Arc::new(AtomicBool::new(false)),
            registrations: vec![],
        };
        for signal in [
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGHUP,
        ] {
            value.registrations.push(signal_hook::flag::register(
                signal,
                value.interrupted.clone(),
            )?);
        }
        Ok(value)
    }
}

impl Drop for RunSignals {
    fn drop(&mut self) {
        for id in self.registrations.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

struct RunChild {
    child: std::process::Child,
    stopped: bool,
}

impl RunChild {
    fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        // Wrappers may exit while descendants still own capture descriptors.
        // Every command has its own group, so cleanup cannot reach StackPulse.
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for RunChild {
    fn drop(&mut self) {
        self.stop();
    }
}

fn captured_output(file: &tempfile::NamedTempFile) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    file.reopen()?.take(OUTPUT_LIMIT).read_to_end(&mut output)?;
    Ok(output)
}

fn failure_details(file: &tempfile::NamedTempFile) -> Result<String> {
    // Compiler errors follow dependency/build logs. Keep the actual final error
    // visible while bounding both disk reads and text rendered by the setup UI.
    let mut file = file.reopen()?;
    file.seek(SeekFrom::End(-(file.metadata()?.len().min(8192) as i64)))?;
    let mut bytes = Vec::new();
    file.take(8192).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let tail: String = text.chars().rev().take(2000).collect();
    Ok(crate::widget::clean(
        &tail.chars().rev().collect::<String>(),
        2000,
    ))
}

pub(crate) fn run(command: &mut Command, label: &str, timeout: Duration) -> Result<Output> {
    let signals = RunSignals::register()?;
    // Regular files avoid pipe backpressure and never wait for descendant EOF.
    // Only a bounded prefix is loaded into memory after the command exits.
    let stdout = tempfile::NamedTempFile::new()?;
    let stderr = tempfile::NamedTempFile::new()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut process = RunChild {
        child: command
            .stdin(Stdio::null())
            .stdout(stdout.reopen()?)
            .stderr(stderr.reopen()?)
            .spawn()
            .with_context(|| format!("Não foi possível executar {label}."))?,
        stopped: false,
    };
    let started = Instant::now();
    loop {
        ensure!(
            !signals.interrupted.load(Ordering::Relaxed),
            "{label} interrompido."
        );
        if let Some(status) = process.child.try_wait()? {
            process.stop();
            let output = Output {
                status,
                stdout: captured_output(&stdout)?,
                stderr: captured_output(&stderr)?,
            };
            ensure!(
                output.status.success(),
                "{label} falhou: {}",
                failure_details(&stderr)?
            );
            return Ok(output);
        }
        if started.elapsed() >= timeout {
            bail!(
                "{label} excedeu o limite de {} segundos.",
                timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn version(executable: &Path) -> Result<String> {
    let output = run(
        Command::new(executable).arg("--version"),
        "ai-memory --version",
        Duration::from_secs(10),
    )?;
    let value = String::from_utf8(output.stdout)?.trim().to_owned();
    ensure!(
        value.starts_with("ai-memory ")
            && value.len() < 200
            && !value.chars().any(char::is_control),
        "Executável não reconhecido como AI-Memory: {}",
        executable.display()
    );
    Ok(value)
}

pub(crate) fn verify_archive(path: &Path, expected: &str) -> Result<()> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    ensure!(
        format!("{:x}", hash.finalize()) == expected,
        "Checksum SHA256 do AI-Memory não confere. Nenhum pacote foi instalado."
    );
    Ok(())
}

pub fn install(prefix: &Path) -> Result<Installation> {
    let prefix = if prefix.is_absolute() {
        prefix.to_owned()
    } else {
        std::env::current_dir()?.join(prefix)
    };
    install_with_search(&prefix, &crate::cli_providers::search_directories())
}

fn install_with_search(prefix: &Path, search: &[PathBuf]) -> Result<Installation> {
    let destination = prefix.join("bin/ai-memory");
    if fs::symlink_metadata(&destination).is_ok() {
        return Ok(Installation { version: version(&destination).context("AI-Memory existente foi preservado; escolha outro --prefix se precisar de nova instalação.")?, executable: destination, reused: true });
    }
    if let Some(executable) =
        crate::cli_providers::resolve_executable(Path::new("ai-memory"), search)
    {
        return Ok(Installation {
            version: version(&executable)?,
            executable,
            reused: true,
        });
    }
    let (asset, checksum) = release(std::env::consts::OS, std::env::consts::ARCH)?;
    let download = tempfile::tempdir()?;
    let archive = download.path().join(asset);
    let url = format!("{PROJECT_URL}/releases/download/v{VERSION}/{asset}");
    println!("Baixando AI-Memory {VERSION} do projeto oficial…");
    run(
        Command::new("curl")
            .args([
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--tlsv1.2",
                "--connect-timeout",
                "15",
                "--max-time",
                "180",
                "--max-filesize",
                "104857600",
                "--output",
            ])
            .arg(&archive)
            .arg(url),
        "download do AI-Memory (curl)",
        Duration::from_secs(185),
    )?;
    verify_archive(&archive, checksum)?;
    publish_archive(prefix, &archive)
}

fn publish_archive(prefix: &Path, archive: &Path) -> Result<Installation> {
    let bundles = prefix.join("share/stackpulse/ai-memory");
    let bundle = bundles.join(format!("v{VERSION}"));
    let destination = prefix.join("bin/ai-memory");
    ensure!(
        fs::symlink_metadata(&bundle).is_err(),
        "O pacote {} já existe. Ele foi preservado; escolha outro --prefix.",
        bundle.display()
    );
    fs::create_dir_all(&bundles)?;
    let stage = tempfile::Builder::new()
        .prefix(".install-")
        .tempdir_in(&bundles)?;
    // Only an archive matching the pinned official hash reaches extraction.
    run(
        Command::new("tar")
            .arg("-xzf")
            .arg(archive)
            .arg("-C")
            .arg(stage.path()),
        "extração do AI-Memory (tar)",
        Duration::from_secs(30),
    )?;
    let binary = stage.path().join("ai-memory");
    ensure!(
        fs::symlink_metadata(&binary)?.file_type().is_file(),
        "Pacote sem executável AI-Memory regular."
    );
    ensure!(
        stage.path().join("LICENSE").is_file() && stage.path().join("hooks").is_dir(),
        "Pacote incompleto: faltam licença ou hooks."
    );
    let actual_version = version(&binary)?;
    ensure!(
        actual_version == format!("ai-memory {VERSION}"),
        "Versão inesperada no pacote: {actual_version}"
    );
    fs::create_dir_all(prefix.join("bin"))?;
    // A symlink creation refuses an existing destination, including a dangling link.
    fs::rename(stage.path(), &bundle)?;
    #[cfg(unix)]
    if let Err(error) = std::os::unix::fs::symlink(bundle.join("ai-memory"), &destination) {
        // This directory was just published by this attempt; no existing binary is removed.
        let _ = fs::remove_dir_all(&bundle);
        return Err(error).context(
            "Destino AI-Memory já existe ou não pôde ser criado; instalação existente preservada.",
        );
    }
    #[cfg(not(unix))]
    bail!("Instalação nativa disponível somente em macOS/Linux.");
    Ok(Installation {
        executable: destination,
        version: actual_version,
        reused: false,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn assert_process_stopped(pid: libc::pid_t) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            // Orphan descendants can briefly remain zombies until the host
            // reaper collects them; they no longer run or own descriptors.
            let output = Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "stat="])
                .output()
                .unwrap();
            let state = String::from_utf8_lossy(&output.stdout);
            if !output.status.success() || state.trim().starts_with('Z') {
                return;
            }
            assert!(Instant::now() < deadline, "process {pid} is still running");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn run_returns_when_wrapper_exits_with_descendant_holding_output_open() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("descendant.pid");
        let started = Instant::now();
        let output = run(
            Command::new("/bin/sh")
                .args([
                    "-c",
                    "sleep 30 & printf '%s\\n' \"$!\" > \"$1\"; printf 'ai-memory 2.2.1\\n'",
                    "memory-test",
                ])
                .arg(&pid_file),
            "wrapper fixture",
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(output.stdout, b"ai-memory 2.2.1\n");
        let pid = fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_process_stopped(pid);
    }

    #[test]
    fn run_captures_bounded_prefix_without_pipe_backpressure() {
        let output = run(
            Command::new("/bin/sh").args([
                "-c",
                "dd if=/dev/zero bs=1024 count=256 2>/dev/null; dd if=/dev/zero bs=1024 count=256 >&2 2>/dev/null",
            ]),
            "output fixture",
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(output.stdout.len(), OUTPUT_LIMIT as usize);
        assert_eq!(output.stderr.len(), OUTPUT_LIMIT as usize);
    }

    #[test]
    fn failed_build_keeps_the_final_error_after_long_compilation_output() {
        let error = run(
            Command::new("/bin/sh").args(["-c", "dd if=/dev/zero bs=1024 count=80 >&2 2>/dev/null; printf '\\nerror: linker unavailable\\n' >&2; exit 1"]),
            "build fixture", Duration::from_secs(2),
        ).unwrap_err().to_string();
        assert!(error.contains("error: linker unavailable"));
        assert!(error.len() < 2200);
    }

    #[test]
    fn run_timeout_kills_group_and_reaps_owned_child() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("processes.pid");
        let started = Instant::now();
        let error = run(
            Command::new("/bin/sh")
                .args([
                    "-c",
                    "sleep 30 & printf '%s %s\\n' \"$$\" \"$!\" > \"$1\"; wait",
                    "memory-test",
                ])
                .arg(&pid_file),
            "timeout fixture",
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(error.to_string().contains("excedeu o limite"));
        assert!(started.elapsed() < Duration::from_secs(2));
        let pids: Vec<libc::pid_t> = fs::read_to_string(pid_file)
            .unwrap()
            .split_whitespace()
            .map(|value| value.parse().unwrap())
            .collect();
        assert_eq!(pids.len(), 2);
        for &pid in &pids {
            assert_process_stopped(pid);
        }
        let mut status = 0;
        let wait = unsafe { libc::waitpid(pids[0], &mut status, libc::WNOHANG) };
        assert_eq!(wait, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn checksum_failure_does_not_publish_anything() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("archive");
        fs::write(&archive, b"corrupt").unwrap();
        assert!(verify_archive(&archive, &"0".repeat(64)).is_err());
        let checksum = format!("{:x}", Sha256::digest(b"corrupt"));
        verify_archive(&archive, &checksum).unwrap();
        assert!(!dir.path().join("bin").exists());
    }

    #[test]
    fn installation_preserves_bundle_and_reuses_existing_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::create_dir_all(source.join("hooks")).unwrap();
        fs::write(source.join("LICENSE"), "test license").unwrap();
        fs::write(source.join("hooks/example"), "test hook").unwrap();
        fs::write(
            source.join("ai-memory"),
            format!("#!/bin/sh\nprintf 'ai-memory {VERSION}\\n'\n"),
        )
        .unwrap();
        fs::set_permissions(source.join("ai-memory"), fs::Permissions::from_mode(0o755)).unwrap();
        let archive = dir.path().join("fixture.tar.gz");
        assert!(
            Command::new("tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(&source)
                .arg(".")
                .status()
                .unwrap()
                .success()
        );
        let prefix = dir.path().join("prefix with spaces ' $(literal)");
        let installed = publish_archive(&prefix, &archive).unwrap();
        assert!(!installed.reused);
        assert!(
            fs::symlink_metadata(&installed.executable)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            installed
                .executable
                .canonicalize()
                .unwrap()
                .parent()
                .unwrap()
                .join("hooks/example")
                .is_file()
        );
        let again = install_with_search(&prefix, &[]).unwrap();
        assert!(again.reused);
        assert_eq!(again.version, installed.version);
        assert!(!prefix.join("bin/injected").exists());
    }

    #[test]
    fn foreign_file_and_dangling_symlink_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("bin")).unwrap();
        let destination = dir.path().join("bin/ai-memory");
        fs::write(&destination, "unrelated").unwrap();
        assert!(install_with_search(dir.path(), &[]).is_err());
        assert_eq!(fs::read_to_string(&destination).unwrap(), "unrelated");
        fs::remove_file(&destination).unwrap();
        std::os::unix::fs::symlink("missing", &destination).unwrap();
        assert!(install_with_search(dir.path(), &[]).is_err());
        assert_eq!(
            fs::read_link(destination).unwrap(),
            PathBuf::from("missing")
        );
    }
}
