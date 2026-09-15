//! Optional, local installation of AI-UsageBar's CLI and terminal interface.
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use crate::memory::{Installation, run, verify_archive};

pub const PROJECT_URL: &str = "https://github.com/akitaonrails/ai-usagebar";
pub const VERSION: &str = "1.17.0";

const BINARIES: [&str; 2] = ["ai-usagebar", "ai-usagebar-tui"];
const SOURCE_SHA256: &str = "e5a80f31e830d31127579bb5b253d845659a3d72a7dd507ba974f6494a63d43b";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Platform {
    LinuxX86,
    LinuxArm,
    MacX86,
    MacArm,
}

/// The release's Linux executables are GNU libc builds, not musl builds.
/// Required symbol versions were checked in both binaries of the pinned assets.
fn compatible_platform(os: &str, arch: &str, glibc: Option<(u32, u32)>) -> Option<Platform> {
    match (os, arch) {
        ("macos", "x86_64") => Some(Platform::MacX86),
        ("macos", "aarch64") => Some(Platform::MacArm),
        ("linux", "x86_64") if glibc.is_some_and(|version| version >= (2, 34)) => {
            Some(Platform::LinuxX86)
        }
        ("linux", "aarch64") if glibc.is_some_and(|version| version >= (2, 18)) => {
            Some(Platform::LinuxArm)
        }
        _ => None,
    }
}

fn version_pair(value: &str) -> Option<(u32, u32)> {
    let mut fields = value.trim().split('.');
    Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
}

fn host_glibc() -> Option<(u32, u32)> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // glibc returns a process-lifetime NUL-terminated static version string.
        let value = unsafe { std::ffi::CStr::from_ptr(libc::gnu_get_libc_version()) };
        value.to_str().ok().and_then(version_pair)
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        None
    }
}

fn host_platform() -> Option<Platform> {
    compatible_platform(std::env::consts::OS, std::env::consts::ARCH, host_glibc())
}

/// Detection does not run programs, touch credentials, or access the network.
pub fn supported() -> bool {
    host_platform().is_some()
}

pub fn platform_description() -> String {
    match host_platform() {
        Some(Platform::MacX86 | Platform::MacArm) => {
            "macOS · compilação local (Rust 1.88+ e CLT)".into()
        }
        Some(Platform::LinuxX86 | Platform::LinuxArm) => {
            format!(
                "Linux {} · CLI/TUI do pacote oficial",
                std::env::consts::ARCH
            )
        }
        None => format!(
            "Instalação indisponível em {}/{}; consulte o projeto oficial.",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    }
}

pub fn print_credits() {
    println!("AI-UsageBar · criado por Fabio Akita (AkitaOnRails)");
    println!(
        "Projeto: {PROJECT_URL}\nCriador: {}",
        crate::memory::CREATOR_URL
    );
}

pub fn select_install(default: bool) -> Result<bool> {
    if !supported() {
        return Ok(false);
    }
    if crate::tui::available() {
        return crate::memory_ui::select_usagebar(default);
    }
    println!("\nMONITOR DE CONSUMO OPCIONAL");
    print_credits();
    println!("{}", platform_description());
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        return ask_install(default);
    }
    println!("[{}] Instalar AI-UsageBar", if default { "x" } else { " " });
    Ok(default)
}

pub fn ask_install(default: bool) -> Result<bool> {
    if !supported() {
        return Ok(false);
    }
    loop {
        print!(
            "Instalar AI-UsageBar? [{}]: ",
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

pub fn print_installation(value: &Installation) {
    println!(
        "AI-UsageBar {} · {}",
        if value.reused {
            "já disponível"
        } else {
            "instalado"
        },
        value.version
    );
    println!(
        "Interface: {}",
        value.executable.with_file_name(BINARIES[1]).display()
    );
    println!("CLI: {}", value.executable.display());
    println!("Criado por Fabio Akita (AkitaOnRails) · {PROJECT_URL}");
    println!("Criador: {}", crate::memory::CREATOR_URL);
    println!(
        "CLI e TUI disponíveis. Painéis do desktop e início automático são configurações separadas."
    );
}

pub fn install(prefix: &Path) -> Result<Installation> {
    // Gate before even searching for executables: a foreign installation on an
    // unsupported host must never be probed or cause a download to start.
    let platform = host_platform().context(platform_description())?;
    let prefix = if prefix.is_absolute() {
        prefix.to_owned()
    } else {
        std::env::current_dir()?.join(prefix)
    };
    install_with_search(
        &prefix,
        &crate::cli_providers::search_directories(),
        Some(platform),
    )
}

fn path_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("Não foi possível verificar {}.", path.display()))
        }
    }
}

fn regular_executable(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("Executável ausente ou link inválido: {}", path.display()))?;
    ensure!(
        metadata.is_file(),
        "Executável não é um arquivo: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o111 != 0,
            "Arquivo sem permissão de execução: {}",
            path.display()
        );
    }
    Ok(())
}

fn verify_binaries(directory: &Path, pinned: bool) -> Result<String> {
    let binary = directory.join(BINARIES[0]);
    let tui = directory.join(BINARIES[1]);
    regular_executable(&binary)?;
    regular_executable(&tui)?;
    let output = run(
        Command::new(&binary).arg("--version"),
        "AI-UsageBar --version",
        Duration::from_secs(10),
    )?;
    let version = String::from_utf8(output.stdout)?.trim().to_owned();
    ensure!(
        version
            .strip_prefix("ai-usagebar ")
            .and_then(version_pair)
            .is_some()
            && version.len() < 200
            && !version.chars().any(char::is_control),
        "Executável não reconhecido como AI-UsageBar: {}",
        binary.display()
    );
    if pinned {
        ensure!(
            version == format!("ai-usagebar {VERSION}"),
            "Versão inesperada no pacote: {version}"
        );
    }
    // Upstream's TUI has no --version flag. --help exits before reading config,
    // credentials, or starting the terminal UI, unlike running it without args.
    let output = run(
        Command::new(&tui).arg("--help"),
        "AI-UsageBar TUI --help",
        Duration::from_secs(10),
    )?;
    ensure!(
        String::from_utf8_lossy(&output.stdout).contains("ai-usagebar-tui"),
        "Interface não reconhecida como AI-UsageBar TUI: {}",
        tui.display()
    );
    Ok(version)
}

fn installed_in(directory: &Path) -> Result<Installation> {
    let version = verify_binaries(directory, false).context(
        "AI-UsageBar existente foi preservado; escolha outro prefixo para uma nova instalação.",
    )?;
    Ok(Installation {
        executable: directory.join(BINARIES[0]),
        version,
        reused: true,
    })
}

fn install_with_search(
    prefix: &Path,
    search: &[PathBuf],
    platform: Option<Platform>,
) -> Result<Installation> {
    let platform = platform.context("AI-UsageBar: plataforma sem instalação compatível.")?;
    let bin = prefix.join("bin");
    let present = [
        path_exists(&bin.join(BINARIES[0]))?,
        path_exists(&bin.join(BINARIES[1]))?,
    ];
    if present.iter().any(|&exists| exists) {
        ensure!(
            present.iter().all(|&exists| exists),
            "Instalação AI-UsageBar incompleta em {}: são necessários CLI e TUI. Arquivos existentes preservados; escolha outro prefixo.",
            bin.display()
        );
        return installed_in(&bin);
    }
    // Only reuse a complete pair from one installation directory. A standalone
    // CLI elsewhere on PATH cannot masquerade as an installed terminal UI.
    for directory in search {
        if BINARIES
            .iter()
            .all(|name| regular_executable(&directory.join(name)).is_ok())
        {
            return installed_in(directory);
        }
    }
    let bundle = bundle_path(prefix);
    ensure!(
        !path_exists(&bundle)?,
        "Pacote {} existente foi preservado; escolha outro prefixo.",
        bundle.display()
    );
    let temporary = tempfile::tempdir()?;
    let source = match platform {
        Platform::LinuxX86 | Platform::LinuxArm => prepare_linux(temporary.path(), platform)?,
        Platform::MacX86 | Platform::MacArm => prepare_macos(temporary.path(), platform)?,
    };
    publish_directory(prefix, &source)
}

fn download(url: &str, archive: &Path, checksum: &str) -> Result<()> {
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
            .arg(archive)
            .arg(url),
        "download do AI-UsageBar (curl)",
        Duration::from_secs(185),
    )?;
    verify_archive(archive, checksum)
        .context("Checksum SHA256 do AI-UsageBar inválido; instalação cancelada.")
}

fn extract(archive: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    run(
        Command::new("tar")
            .arg("-xzf")
            .arg(archive)
            .arg("-C")
            .arg(destination),
        "extração do AI-UsageBar (tar)",
        Duration::from_secs(30),
    )?;
    Ok(())
}

fn prepare_linux(temporary: &Path, platform: Platform) -> Result<PathBuf> {
    let (asset, checksum) = match platform {
        Platform::LinuxX86 => (
            "ai-usagebar-linux-x86_64.tar.gz",
            "99a5f1b06ec5558c50af4cd021ce1500c629af169e92e236f629cae1d30c214a",
        ),
        Platform::LinuxArm => (
            "ai-usagebar-linux-aarch64.tar.gz",
            "7485d8c5f3f9472c94425be9604dd5644b75cb6162d636f57885ee70361223f0",
        ),
        _ => bail!("AI-UsageBar: pacote Linux incompatível."),
    };
    println!("Baixando AI-UsageBar {VERSION} do projeto oficial…");
    let archive = temporary.join(asset);
    download(
        &format!("{PROJECT_URL}/releases/download/v{VERSION}/{asset}"),
        &archive,
        checksum,
    )?;
    println!("Checksum confirmado. Preparando CLI e TUI…");
    let extracted = temporary.join("extracted");
    extract(&archive, &extracted)?;
    Ok(extracted)
}

#[derive(Debug, PartialEq, Eq)]
enum RustTools {
    Current,
    Installed(String),
}

impl RustTools {
    fn command(&self, binary: &str) -> Command {
        let mut command = match self {
            Self::Current => Command::new(binary),
            Self::Installed(toolchain) => {
                let mut command = Command::new("rustup");
                // Only names observed in `rustup toolchain list` reach this path.
                // `rustup run` without --install refuses missing toolchains.
                command.args(["run", toolchain, binary]);
                command
            }
        };
        // Also keep a rustup proxy on PATH from provisioning a missing default
        // or environment-selected toolchain during the current-tool probe.
        command.env("RUSTUP_AUTO_INSTALL", "0");
        command
    }
}

fn compatible_rust_version(value: &str, program: &str) -> bool {
    let mut fields = value.split_whitespace();
    if fields.next() != Some(program) {
        return false;
    }
    let Some(version) = fields.next() else {
        return false;
    };
    let components: Vec<_> = version.split('.').collect();
    components.len() == 3
        && components.iter().all(|part| part.parse::<u32>().is_ok())
        && version_pair(version).is_some_and(|version| version >= (1, 88))
}

fn installed_rust_candidates(listing: &str, target: &str) -> Vec<String> {
    let suffix = format!("-{target}");
    let mut candidates = Vec::new();
    for line in listing.lines().take(128) {
        let Some(name) = line.split_whitespace().next() else {
            continue;
        };
        let Some(channel) = name.strip_suffix(&suffix) else {
            continue;
        };
        let numbered = channel
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit()));
        if channel != "stable"
            && !(numbered && version_pair(channel).is_some_and(|version| version >= (1, 88)))
        {
            continue;
        }
        if !candidates.iter().any(|candidate| candidate == name) {
            candidates.push(name.to_owned());
        }
        if candidates.len() == 8 {
            break;
        }
    }
    candidates
}

fn choose_rust_tools(
    current_compatible: bool,
    listing: &str,
    target: &str,
    mut compatible: impl FnMut(&str) -> Result<bool>,
) -> Result<Option<RustTools>> {
    if current_compatible {
        return Ok(Some(RustTools::Current));
    }
    for candidate in installed_rust_candidates(listing, target) {
        if compatible(&candidate)? {
            return Ok(Some(RustTools::Installed(candidate)));
        }
    }
    Ok(None)
}

fn probe_rust(command: &mut Command, label: &str) -> Result<Option<String>> {
    match run(command, label, Duration::from_secs(5)) {
        Ok(output) => Ok(String::from_utf8(output.stdout).ok()),
        // A failed/missing compiler can have a compatible installed alternative;
        // cancellation must still abort instead of continuing the search.
        Err(error)
            if error
                .chain()
                .any(|cause| cause.to_string().contains("interrompido")) =>
        {
            Err(error)
        }
        Err(_) => Ok(None),
    }
}

fn rust_tools_compatible(tools: &RustTools, temporary: &Path) -> Result<bool> {
    for program in ["rustc", "cargo"] {
        let output = probe_rust(
            tools
                .command(program)
                .arg("--version")
                .current_dir(temporary),
            &format!("Verificação de {program}"),
        )?;
        if !output.is_some_and(|value| compatible_rust_version(&value, program)) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn select_rust_tools(temporary: &Path, target: &str) -> Result<RustTools> {
    if rust_tools_compatible(&RustTools::Current, temporary)? {
        return Ok(RustTools::Current);
    }
    println!("Procurando uma versão compatível do Rust já instalada…");
    let listing = probe_rust(
        Command::new("rustup")
            .args(["toolchain", "list"])
            .env("RUSTUP_AUTO_INSTALL", "0")
            .current_dir(temporary),
        "Toolchains Rust já instaladas",
    )?
    .unwrap_or_default();
    let tools = choose_rust_tools(false, &listing, target, |candidate| {
        rust_tools_compatible(&RustTools::Installed(candidate.to_owned()), temporary)
    })?
    .context("AI-UsageBar requer Rust 1.88+ e Cargo. Instale uma toolchain estável compatível e tente novamente; o StackPulse não instala Rust nem altera a versão padrão.")?;
    if let RustTools::Installed(name) = &tools {
        println!("Usando Rust já instalado: {name}. A versão padrão será preservada.");
    }
    Ok(tools)
}

fn prepare_macos(temporary: &Path, platform: Platform) -> Result<PathBuf> {
    let target = match platform {
        Platform::MacX86 => "x86_64-apple-darwin",
        Platform::MacArm => "aarch64-apple-darwin",
        _ => bail!("AI-UsageBar: compilação macOS incompatível."),
    };
    println!("Verificando Rust e Command Line Tools para compilar AI-UsageBar…");
    let rust = select_rust_tools(temporary, target)?;
    run(
        Command::new("xcrun").args(["--find", "clang"]),
        "Command Line Tools do macOS",
        Duration::from_secs(10),
    )?;
    println!("Baixando o código oficial do AI-UsageBar {VERSION}…");
    let archive = temporary.join("ai-usagebar.crate");
    download(
        &format!("https://static.crates.io/crates/ai-usagebar/ai-usagebar-{VERSION}.crate"),
        &archive,
        SOURCE_SHA256,
    )?;
    let extracted = temporary.join("source");
    extract(&archive, &extracted)?;
    let source = extracted.join(format!("ai-usagebar-{VERSION}"));
    let compiled = temporary.join("compiled");
    println!("Compilando CLI e TUI localmente. Esta etapa pode levar alguns minutos…");
    run(
        rust.command("cargo")
            .args(["install", "--path"])
            .arg(&source)
            .arg("--locked")
            .arg("--root")
            .arg(&compiled)
            .args([
                "--target",
                target,
                "--bin",
                BINARIES[0],
                "--bin",
                BINARIES[1],
            ])
            .env("CARGO_TARGET_DIR", temporary.join("target"))
            .current_dir(&source),
        "compilação do AI-UsageBar (Cargo)",
        Duration::from_secs(1200),
    )?;
    let prepared = compiled.join("bin");
    fs::copy(source.join("LICENSE"), prepared.join("LICENSE"))?;
    fs::copy(source.join("README.md"), prepared.join("README.md"))?;
    Ok(prepared)
}

fn bundle_path(prefix: &Path) -> PathBuf {
    prefix
        .join("share/stackpulse/ai-usagebar")
        .join(format!("v{VERSION}"))
}

fn publish_directory(prefix: &Path, source: &Path) -> Result<Installation> {
    let bundle = bundle_path(prefix);
    let bin = prefix.join("bin");
    ensure!(
        !path_exists(&bundle)?,
        "Pacote {} existente foi preservado.",
        bundle.display()
    );
    for name in BINARIES {
        ensure!(
            !path_exists(&bin.join(name))?,
            "Destino {} existente foi preservado.",
            bin.join(name).display()
        );
        ensure!(
            fs::symlink_metadata(source.join(name))?
                .file_type()
                .is_file(),
            "Pacote sem executável regular {name}."
        );
    }
    ensure!(
        fs::symlink_metadata(source.join("LICENSE"))?
            .file_type()
            .is_file(),
        "Pacote sem licença regular."
    );
    println!("Verificando CLI e TUI do AI-UsageBar…");
    let version = verify_binaries(source, true)?;
    let bundles = bundle
        .parent()
        .context("Diretório de pacotes indisponível.")?;
    fs::create_dir_all(bundles)?;
    fs::create_dir_all(&bin)?;
    let staged = tempfile::Builder::new()
        .prefix(".install-")
        .tempdir_in(bundles)?;
    for name in [BINARIES[0], BINARIES[1], "LICENSE"] {
        fs::copy(source.join(name), staged.path().join(name))?;
    }
    for name in ["README.md", "config.example.toml"] {
        if fs::symlink_metadata(source.join(name))
            .is_ok_and(|metadata| metadata.file_type().is_file())
        {
            fs::copy(source.join(name), staged.path().join(name))?;
        }
    }
    // create_dir refuses existing paths, unlike rename onto an empty directory.
    // Both public links are also created exclusively; no foreign file is replaced.
    fs::create_dir(&bundle).context("Pacote AI-UsageBar existente foi preservado.")?;
    let mut links = Vec::new();
    let result = (|| -> Result<()> {
        for entry in fs::read_dir(staged.path())? {
            let entry = entry?;
            fs::hard_link(entry.path(), bundle.join(entry.file_name()))?;
        }
        for name in BINARIES {
            let destination = bin.join(name);
            let target = bundle.join(name);
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &destination)?;
            #[cfg(not(unix))]
            bail!("Instalação nativa do AI-UsageBar disponível somente em macOS/Linux.");
            links.push((destination, target));
        }
        Ok(())
    })();
    if let Err(error) = result {
        for (destination, target) in links {
            if fs::read_link(&destination).is_ok_and(|actual| actual == target) {
                let _ = fs::remove_file(destination);
            }
        }
        // Only the bundle created by this attempt is removed on publication failure.
        let _ = fs::remove_dir_all(&bundle);
        return Err(error)
            .context("Não foi possível publicar AI-UsageBar; arquivos existentes preservados.");
    }
    Ok(Installation {
        executable: bin.join(BINARIES[0]),
        version,
        reused: false,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn fixture(directory: &Path) {
        fs::create_dir_all(directory).unwrap();
        for (name, flag, output) in [
            (BINARIES[0], "--version", format!("ai-usagebar {VERSION}")),
            (
                BINARIES[1],
                "--help",
                "usage: ai-usagebar-tui [--config <PATH>]".into(),
            ),
        ] {
            fs::write(
                directory.join(name),
                format!("#!/bin/sh\n[ \"$1\" = '{flag}' ] || exit 9\nprintf '%s\\n' '{output}'\n"),
            )
            .unwrap();
            fs::set_permissions(directory.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(directory.join("LICENSE"), "fixture license").unwrap();
    }

    #[test]
    fn platform_gate_checks_architecture_and_actual_glibc() {
        for arch in ["x86_64", "aarch64"] {
            assert!(compatible_platform("macos", arch, None).is_some());
            assert!(compatible_platform("linux", arch, Some((2, 40))).is_some());
            assert_eq!(compatible_platform("linux", arch, None), None);
            assert_eq!(compatible_platform("windows", arch, None), None);
        }
        assert_eq!(compatible_platform("linux", "x86_64", Some((2, 33))), None);
        assert_eq!(
            compatible_platform("linux", "x86_64", Some((2, 34))),
            Some(Platform::LinuxX86)
        );
        assert_eq!(compatible_platform("linux", "aarch64", Some((2, 17))), None);
        assert_eq!(
            compatible_platform("linux", "aarch64", Some((2, 18))),
            Some(Platform::LinuxArm)
        );
        for os in ["linux", "macos", "freebsd"] {
            assert_eq!(compatible_platform(os, "arm", Some((2, 40))), None);
        }
        assert_eq!(version_pair("unknown"), None);
    }

    #[test]
    fn compatible_current_rust_is_preferred_without_probing_alternatives() {
        assert!(!compatible_rust_version("rustc 1.67.0 (abc)", "rustc"));
        assert!(compatible_rust_version("rustc 1.88.0 (abc)", "rustc"));
        assert!(compatible_rust_version("cargo 1.93.1 (abc)", "cargo"));
        assert!(!compatible_rust_version(
            "rustc 1.94.0-nightly (abc)",
            "rustc"
        ));
        let result = choose_rust_tools(true, "", "aarch64-apple-darwin", |_| {
            panic!("compatible current toolchain must not probe alternatives")
        })
        .unwrap();
        assert_eq!(result, Some(RustTools::Current));
    }

    #[test]
    fn old_default_uses_only_observed_stable_native_toolchain_for_rustc_and_cargo() {
        let listing = "stable-aarch64-apple-darwin (default)\n\
                       nightly-aarch64-apple-darwin\n\
                       1.67.0-aarch64-apple-darwin\n\
                       1.93.1-x86_64-apple-darwin\n\
                       1.93.1-aarch64-apple-darwin (active)\n\
                       --install\n";
        let mut probes = Vec::new();
        let selected = choose_rust_tools(false, listing, "aarch64-apple-darwin", |name| {
            probes.push(name.to_owned());
            Ok(name == "1.93.1-aarch64-apple-darwin")
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            probes,
            ["stable-aarch64-apple-darwin", "1.93.1-aarch64-apple-darwin"]
        );
        for program in ["rustc", "cargo"] {
            let command = selected.command(program);
            assert_eq!(command.get_program(), "rustup");
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(args, ["run", "1.93.1-aarch64-apple-darwin", program]);
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == "RUSTUP_AUTO_INSTALL"
                        && value == Some(std::ffi::OsStr::new("0")))
            );
        }
    }

    #[test]
    fn toolchain_search_is_bounded_and_propagates_cancellation() {
        let listing = (88..110)
            .map(|minor| format!("1.{minor}.0-aarch64-apple-darwin\n"))
            .collect::<String>();
        assert_eq!(
            installed_rust_candidates(&listing, "aarch64-apple-darwin").len(),
            8
        );
        let mut calls = 0;
        let error = choose_rust_tools(false, &listing, "aarch64-apple-darwin", |_| {
            calls += 1;
            bail!("interrompido")
        })
        .unwrap_err();
        assert!(error.to_string().contains("interrompido"));
        assert_eq!(calls, 1);
        assert_eq!(
            choose_rust_tools(false, "", "aarch64-apple-darwin", |_| Ok(true)).unwrap(),
            None
        );
    }

    #[test]
    fn unsupported_platform_never_executes_existing_binaries() {
        let temporary = tempfile::tempdir().unwrap();
        let bin = temporary.path().join("bin");
        fixture(&bin);
        for name in BINARIES {
            fs::write(bin.join(name), "#!/bin/sh\ntouch \"$0.executed\"\n").unwrap();
        }
        assert!(install_with_search(temporary.path(), &[], None).is_err());
        for name in BINARIES {
            assert!(!bin.join(format!("{name}.executed")).exists());
        }
        assert!(!temporary.path().join("share").exists());
    }

    #[test]
    fn installs_complete_pair_with_license_and_reuses_without_network() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fixture(&source);
        let prefix = temporary.path().join("prefix with spaces ' $(literal)");
        let installed = publish_directory(&prefix, &source).unwrap();
        assert!(!installed.reused);
        for name in BINARIES {
            assert!(
                fs::symlink_metadata(prefix.join("bin").join(name))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
        assert_eq!(
            fs::read_to_string(bundle_path(&prefix).join("LICENSE")).unwrap(),
            "fixture license"
        );
        let again = install_with_search(&prefix, &[], Some(Platform::MacArm)).unwrap();
        assert!(again.reused);
        assert_eq!(again.version, installed.version);
    }

    #[test]
    fn reuses_complete_external_installation_without_writing_prefix() {
        let temporary = tempfile::tempdir().unwrap();
        let external = temporary.path().join("existing bin");
        let prefix = temporary.path().join("unused prefix");
        fixture(&external);
        let result = install_with_search(
            &prefix,
            std::slice::from_ref(&external),
            Some(Platform::MacArm),
        )
        .unwrap();
        assert!(result.reused);
        assert_eq!(result.executable, external.join(BINARIES[0]));
        assert!(!prefix.exists());
    }

    #[test]
    fn preexisting_bundle_is_preserved_before_installation() {
        let temporary = tempfile::tempdir().unwrap();
        let bundle = bundle_path(temporary.path());
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join("keep"), "existing bundle").unwrap();
        assert!(install_with_search(temporary.path(), &[], Some(Platform::MacArm)).is_err());
        assert_eq!(
            fs::read_to_string(bundle.join("keep")).unwrap(),
            "existing bundle"
        );
        assert!(!temporary.path().join("bin").exists());
    }

    #[test]
    fn incomplete_prefix_pair_is_preserved_without_probe_or_download() {
        let temporary = tempfile::tempdir().unwrap();
        let bin = temporary.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join(BINARIES[0]), "preserve backend").unwrap();
        assert!(install_with_search(temporary.path(), &[], Some(Platform::MacArm)).is_err());
        assert_eq!(
            fs::read_to_string(bin.join(BINARIES[0])).unwrap(),
            "preserve backend"
        );
        assert!(!bin.join(BINARIES[1]).exists());
        assert!(!temporary.path().join("share").exists());
    }

    #[test]
    fn foreign_files_and_dangling_links_are_never_replaced() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fixture(&source);
        for (index, name) in BINARIES.iter().enumerate() {
            let prefix = temporary.path().join(format!("prefix-{index}"));
            fs::create_dir_all(prefix.join("bin")).unwrap();
            let destination = prefix.join("bin").join(name);
            fs::write(&destination, "unrelated").unwrap();
            assert!(publish_directory(&prefix, &source).is_err());
            assert_eq!(fs::read_to_string(&destination).unwrap(), "unrelated");
            fs::remove_file(&destination).unwrap();
            symlink("missing", &destination).unwrap();
            assert!(publish_directory(&prefix, &source).is_err());
            assert_eq!(
                fs::read_link(&destination).unwrap(),
                PathBuf::from("missing")
            );
            assert!(!bundle_path(&prefix).exists());
        }
    }

    #[test]
    fn incomplete_or_wrong_version_package_is_not_published() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let prefix = temporary.path().join("prefix");
        fixture(&source);
        fs::remove_file(source.join(BINARIES[1])).unwrap();
        assert!(publish_directory(&prefix, &source).is_err());
        fixture(&source);
        fs::remove_file(source.join("LICENSE")).unwrap();
        assert!(publish_directory(&prefix, &source).is_err());
        fixture(&source);
        fs::write(
            source.join(BINARIES[0]),
            "#!/bin/sh\nprintf 'ai-usagebar 0.1.0\\n'\n",
        )
        .unwrap();
        assert!(publish_directory(&prefix, &source).is_err());
        assert!(!prefix.exists());
    }
}
