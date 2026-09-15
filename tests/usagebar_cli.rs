use ai_token_timeline::usagebar;
use std::process::{Command, Stdio};

#[test]
fn compatibility_probe_is_read_only_and_default_selection_does_not_install() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().canonicalize().unwrap();
    let prefix = cwd.join("not-created");
    let config = cwd.join("invalid.json");
    std::fs::write(&config, "invalid config must not be read").unwrap();
    let make_command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-token-timeline"));
        command
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .arg("--allow-workspace")
            .arg(&cwd)
            .arg("--db")
            .arg(cwd.join("db.sqlite"))
            .arg("--config")
            .arg(&config)
            .arg("usagebar");
        command
    };
    let output = make_command().arg("supported").output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(if usagebar::supported() { 0 } else { 1 })
    );
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    let output = make_command()
        .args(["install", "--select", "--prefix"])
        .arg(&prefix)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(if usagebar::supported() { 10 } else { 1 })
    );
    if usagebar::supported() {
        assert!(String::from_utf8_lossy(&output.stdout).contains("[ ] Instalar AI-UsageBar"));
    }
    assert!(!prefix.exists());
    assert!(!cwd.join("db.sqlite").exists());
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "invalid config must not be read"
    );
}
