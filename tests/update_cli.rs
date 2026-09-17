#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
};

#[test]
fn installed_cli_updates_from_an_unrelated_directory_and_preserves_failed_build() {
    let root = tempfile::tempdir().unwrap();
    let prefix = root.path().join("custom prefix");
    let executable = prefix.join("bin/stackpulse");
    let manifest = prefix.join("share/stackpulse/install.manifest");
    let source = root.path().join("source ' $(literal) `literal`");
    let elsewhere = root.path().join("unrelated project");
    let home = root.path().join("home");
    let tools = root.path().join("tools");
    for dir in [
        executable.parent().unwrap(),
        manifest.parent().unwrap(),
        &source,
        &elsewhere,
        &home,
        &tools,
    ] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::copy(env!("CARGO_BIN_EXE_ai-token-timeline"), &executable).unwrap();
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
    let checksum = Command::new("cksum")
        .stdin(Stdio::from(fs::File::open(&executable).unwrap()))
        .output()
        .unwrap();
    fs::write(
        &manifest,
        format!(
            "stackpulse-installer-v1\n{}{}\n",
            String::from_utf8(checksum.stdout).unwrap(),
            source.display()
        ),
    )
    .unwrap();
    fs::write(tools.join("cargo"), r#"#!/bin/sh
set -eu
[ "${FAIL_BUILD:-0}" = 0 ] || exit 42
target=
while [ "$#" -gt 0 ]; do
  case "$1" in --target-dir) target=$2; shift 2 ;; *) shift ;; esac
done
mkdir -p "$target/release"
printf '#!/bin/sh\n[ "$1" = --version ] || exit 88\nprintf "stackpulse fixture-updated\\n"\n' > "$target/release/ai-token-timeline"
chmod +x "$target/release/ai-token-timeline"
"#).unwrap();
    fs::set_permissions(tools.join("cargo"), fs::Permissions::from_mode(0o755)).unwrap();
    let link = root.path().join("stackpulse-link");
    std::os::unix::fs::symlink(&executable, &link).unwrap();
    let invoke = |failure: bool| {
        Command::new(&link)
            .arg("update")
            .env("HOME", &home)
            .env("PATH", format!("{}:/usr/bin:/bin", tools.display()))
            .env("FAIL_BUILD", if failure { "1" } else { "0" })
            .current_dir(&elsewhere)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let before = fs::read(&executable).unwrap();
    let manifest_before = fs::read(&manifest).unwrap();
    let failure = invoke(true);
    assert!(!failure.status.success());
    assert!(
        String::from_utf8_lossy(&failure.stderr).contains("compilação falhou"),
        "{}",
        String::from_utf8_lossy(&failure.stderr)
    );
    assert_eq!(fs::read(&executable).unwrap(), before);
    assert_eq!(fs::read(&manifest).unwrap(), manifest_before);
    let result = invoke(false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("StackPulse atualizado"));
    let version = Command::new(&executable).arg("--version").output().unwrap();
    assert_eq!(version.stdout, b"stackpulse fixture-updated\n");
    assert_eq!(
        fs::read_dir(&home).unwrap().count(),
        0,
        "update must not create configuration, database or workspace permissions"
    );
    assert_eq!(
        fs::read_dir(&elsewhere).unwrap().count(),
        0,
        "update must not modify the invoking project"
    );
    assert!(link.is_symlink());
}
