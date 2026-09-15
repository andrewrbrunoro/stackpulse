use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
};

fn command(cwd: &Path, result: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ai-token-timeline"));
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .env("PATH", "/not-present")
        .arg("--allow-workspace")
        .arg(cwd)
        .arg("--config")
        .arg(cwd.join("invalid-config.json"))
        .arg("--db")
        .arg(cwd.join("not-created.sqlite"))
        .args(["plugins", "select", "--result-file"])
        .arg(result);
    cmd
}

#[test]
fn combined_selection_keeps_defaults_and_explicit_choices_without_installing() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().canonicalize().unwrap();
    fs::write(
        cwd.join("invalid-config.json"),
        "unreadable as configuration",
    )
    .unwrap();
    let result = cwd.join("choices with spaces");
    let output = command(&cwd, &result).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&result).unwrap(),
        "memory=with\nusagebar=without\n"
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("[x] AI-Memory"));
    if ai_token_timeline::usagebar::supported() {
        assert!(text.contains("[ ] AI-UsageBar"));
    }
    assert!(!cwd.join("not-created.sqlite").exists());
    for (index, memory, usagebar) in [
        (0, "without", "without"),
        (1, "with", "without"),
        (2, "without", "with"),
    ] {
        let result = cwd.join(format!("explicit-{index}"));
        let output = command(&cwd, &result)
            .args(["--memory", memory, "--usagebar", usagebar])
            .output()
            .unwrap();
        if usagebar == "with" && !ai_token_timeline::usagebar::supported() {
            assert!(!output.status.success() && !result.exists());
        } else {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                fs::read_to_string(&result).unwrap(),
                format!("memory={memory}\nusagebar={usagebar}\n")
            );
        }
    }
    assert!(!cwd.join("not-created.sqlite").exists());
    assert!(!cwd.join("providers").exists());
}

#[test]
fn selection_never_replaces_existing_result_or_symlink() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().canonicalize().unwrap();
    let result = cwd.join("result");
    fs::write(&result, "keep this file").unwrap();
    let output = command(&cwd, &result).output().unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(&result).unwrap(), "keep this file");
    #[cfg(unix)]
    {
        let link = cwd.join("result-link");
        std::os::unix::fs::symlink(&result, &link).unwrap();
        let output = command(&cwd, &link).output().unwrap();
        assert!(!output.status.success());
        assert_eq!(fs::read_link(&link).unwrap(), result);
        assert_eq!(fs::read_to_string(&result).unwrap(), "keep this file");
    }
}
