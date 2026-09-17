//! Updates a managed installation from its local source checkout.
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Debug)]
struct Installation {
    prefix: PathBuf,
    source: PathBuf,
}

impl Installation {
    fn locate(executable: &Path, source: Option<&Path>, legacy_source: &Path) -> Result<Self> {
        let executable = executable
            .canonicalize()
            .context("Não foi possível localizar o executável do StackPulse.")?;
        let bin = executable
            .parent()
            .context("Executável sem pasta de instalação.")?;
        ensure!(
            executable
                .file_name()
                .is_some_and(|name| name == "stackpulse")
                && bin.file_name().is_some_and(|name| name == "bin"),
            "Update requer uma instalação criada pelo setup.sh (PREFIX/bin/stackpulse)."
        );
        let prefix = bin
            .parent()
            .context("Prefixo da instalação não encontrado.")?
            .to_owned();
        let manifest = fs::read_to_string(prefix.join("share/stackpulse/install.manifest"))
            .context("Manifesto da instalação não encontrado. Update requer uma instalação gerenciada pelo setup.sh.")?;
        let mut lines = manifest.lines();
        ensure!(
            lines.next() == Some("stackpulse-installer-v1"),
            "Manifesto de instalação não reconhecido."
        );
        let saved_checksum = lines
            .next()
            .context("Manifesto de instalação incompleto.")?;
        let checksum = Command::new("cksum")
            .stdin(Stdio::from(File::open(&executable)?))
            .output()
            .context("Não foi possível verificar a instalação com cksum.")?;
        ensure!(
            checksum.status.success(),
            "Falha ao verificar a integridade da instalação."
        );
        ensure!(
            String::from_utf8_lossy(&checksum.stdout).trim() == saved_checksum,
            "O executável instalado foi alterado. Update não substituirá uma instalação não reconhecida."
        );
        let recorded_source = lines
            .next()
            .filter(|line| !line.is_empty())
            .map(PathBuf::from);
        ensure!(
            lines.next().is_none(),
            "Manifesto de instalação contém dados inesperados."
        );
        let source = source
            .map(Path::to_owned)
            .or(recorded_source)
            .unwrap_or_else(|| legacy_source.to_owned());
        let source = source.canonicalize().with_context(|| format!(
            "Fontes locais indisponíveis em {}. Use stackpulse update --source DIR para indicar o clone.", source.display()
        ))?;
        ensure!(
            [
                "setup.sh",
                "Cargo.toml",
                "Cargo.lock",
                "rust-toolchain.toml"
            ]
            .iter()
            .all(|file| source.join(file).is_file()),
            "A pasta {} não contém as fontes completas do StackPulse. Use stackpulse update --source DIR.",
            source.display()
        );
        let cargo: toml::Value = toml::from_str(&fs::read_to_string(source.join("Cargo.toml"))?)
            .context("Cargo.toml das fontes é inválido.")?;
        ensure!(
            cargo
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
                == Some("ai-token-timeline"),
            "As fontes indicadas não pertencem ao StackPulse."
        );
        Ok(Self { prefix, source })
    }

    fn installer(&self) -> Command {
        let mut command = Command::new("sh");
        command
            .arg(self.source.join("setup.sh"))
            .arg("--prefix")
            .arg(&self.prefix)
            .args(["--no-path", "--without-memory", "--without-usagebar"])
            .current_dir(&self.source)
            .stdin(Stdio::null());
        command
    }
}

/// Rebuilds local sources; never fetches or modifies the source checkout with Git.
pub fn run(source: Option<&Path>) -> Result<()> {
    let installation = Installation::locate(
        &std::env::current_exe()?,
        source,
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )?;
    println!(
        "StackPulse · atualizando a partir de {}",
        installation.source.display()
    );
    println!(
        "Destino: {}",
        installation.prefix.join("bin/stackpulse").display()
    );
    let status = installation
        .installer()
        .status()
        .context("Não foi possível iniciar a atualização.")?;
    ensure!(
        status.success(),
        "A atualização não foi concluída ({status}). Consulte o erro acima."
    );
    println!("StackPulse atualizado. Abra novamente o StackPulse para usar a nova versão.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Fixture {
        root: tempfile::TempDir,
        executable: PathBuf,
        source: PathBuf,
        manifest: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("source with ' spaces $() `literal`");
            let prefix = root.path().join("custom prefix ' $() `literal`");
            let executable = prefix.join("bin/stackpulse");
            let manifest = prefix.join("share/stackpulse/install.manifest");
            fs::create_dir_all(&source).unwrap();
            fs::create_dir_all(executable.parent().unwrap()).unwrap();
            fs::create_dir_all(manifest.parent().unwrap()).unwrap();
            for file in [
                "setup.sh",
                "Cargo.toml",
                "Cargo.lock",
                "rust-toolchain.toml",
            ] {
                fs::copy(
                    Path::new(env!("CARGO_MANIFEST_DIR")).join(file),
                    source.join(file),
                )
                .unwrap();
            }
            fs::write(&executable, "#!/bin/sh\nprintf 'stackpulse previous\\n'\n").unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            let fixture = Self {
                root,
                executable,
                source,
                manifest,
            };
            fixture.write_manifest(true);
            fixture
        }

        fn write_manifest(&self, include_source: bool) {
            let checksum = Command::new("cksum")
                .stdin(Stdio::from(File::open(&self.executable).unwrap()))
                .output()
                .unwrap();
            fs::write(
                &self.manifest,
                format!(
                    "stackpulse-installer-v1\n{}{}",
                    String::from_utf8(checksum.stdout).unwrap(),
                    if include_source {
                        format!("{}\n", self.source.display())
                    } else {
                        String::new()
                    }
                ),
            )
            .unwrap();
        }

        fn locate(&self) -> Result<Installation> {
            Installation::locate(
                &self.executable,
                None,
                Path::new("/missing/legacy/checkout"),
            )
        }

        fn installer(&self, failure: bool) -> Command {
            let tools = self.root.path().join("tools");
            fs::create_dir_all(&tools).unwrap();
            let cargo = tools.join("cargo");
            fs::write(&cargo, r#"#!/bin/sh
set -eu
[ "${FAIL_BUILD:-0}" = 0 ] || exit 42
target=
while [ "$#" -gt 0 ]; do
    case "$1" in --target-dir) target=$2; shift 2 ;; *) shift ;; esac
done
mkdir -p "$target/release"
printf '#!/bin/sh\n[ "$1" = --version ] || exit 88\nprintf "stackpulse updated\\n"\n' > "$target/release/ai-token-timeline"
chmod +x "$target/release/ai-token-timeline"
"#).unwrap();
            fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
            let mut command = self.locate().unwrap().installer();
            command
                .env("PATH", format!("{}:/usr/bin:/bin", tools.display()))
                .env("FAIL_BUILD", if failure { "1" } else { "0" })
                .env("HOME", self.root.path())
                .env("SHELL", "/bin/zsh");
            command
        }
    }

    #[test]
    fn finds_recorded_source_and_real_prefix_through_symlink() {
        let fixture = Fixture::new();
        let link = fixture.root.path().join("linked-command");
        std::os::unix::fs::symlink(&fixture.executable, &link).unwrap();
        let install = Installation::locate(&link, None, Path::new("/missing")).unwrap();
        assert_eq!(install.source, fixture.source.canonicalize().unwrap());
        assert_eq!(
            install.prefix.join("bin/stackpulse"),
            fixture.executable.canonicalize().unwrap()
        );
    }

    #[test]
    fn legacy_manifest_uses_compile_time_source_and_override_recovers_moved_clone() {
        let fixture = Fixture::new();
        fixture.write_manifest(false);
        assert!(Installation::locate(&fixture.executable, None, &fixture.source).is_ok());
        fixture.write_manifest(true);
        let moved = fixture.root.path().join("moved source");
        fs::rename(&fixture.source, &moved).unwrap();
        assert!(
            fixture
                .locate()
                .unwrap_err()
                .to_string()
                .contains("--source")
        );
        assert!(
            Installation::locate(&fixture.executable, Some(&moved), Path::new("/missing")).is_ok()
        );
    }

    #[test]
    fn rejects_changed_binary_missing_manifest_and_wrong_checkout() {
        let fixture = Fixture::new();
        fs::write(&fixture.executable, "changed").unwrap();
        assert!(fixture.locate().is_err());
        fixture.write_manifest(true);
        fs::write(
            fixture.source.join("Cargo.toml"),
            "[package]\nname='other'\n",
        )
        .unwrap();
        assert!(fixture.locate().is_err());
        fs::remove_file(&fixture.manifest).unwrap();
        assert!(fixture.locate().is_err());
    }

    #[test]
    fn failed_build_preserves_binary_and_manifest() {
        let fixture = Fixture::new();
        let binary = fs::read(&fixture.executable).unwrap();
        let manifest = fs::read(&fixture.manifest).unwrap();
        let result = fixture.installer(true).output().unwrap();
        assert!(!result.status.success());
        assert_eq!(binary, fs::read(&fixture.executable).unwrap());
        assert_eq!(manifest, fs::read(&fixture.manifest).unwrap());
    }

    #[test]
    fn update_replaces_binary_in_custom_prefix_without_touching_user_data_or_plugins() {
        let fixture = Fixture::new();
        let prefix = fixture.executable.parent().unwrap().parent().unwrap();
        let markers = [
            fixture.root.path().join(".zshrc"),
            fixture.root.path().join("config.json"),
            prefix.join("bin/ai-memory"),
            prefix.join("bin/ai-usagebar"),
        ];
        for marker in &markers {
            fs::write(marker, "preserve").unwrap();
        }
        let result = fixture.installer(false).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(fixture.locate().is_ok());
        let version = Command::new(&fixture.executable)
            .arg("--version")
            .output()
            .unwrap();
        assert_eq!(version.stdout, b"stackpulse updated\n");
        for marker in markers {
            assert_eq!(fs::read_to_string(marker).unwrap(), "preserve");
        }
        assert!(!fixture.root.path().join(".local").exists());
    }
}
