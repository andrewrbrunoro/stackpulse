use ai_token_timeline::profiles::Settings;
use std::{fs, process::Command};

#[test]
fn compare_rejects_insufficient_duplicate_and_unknown_profiles_before_execution() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let config = root.join("config.json");
    let all = root.join("providers/project/all");
    fs::create_dir_all(&all).unwrap();
    Settings {
        schema_version: 1,
        client: Default::default(),
        provider: "openai".into(),
        model: "helper".into(),
        effort: "low".into(),
        executable: root.join("must-not-execute"),
        providers_root: root.join("providers"),
        default_profile: None,
        max_agents: 4,
        ai_memory: false,
        ai_usagebar: false,
    }
    .save(&config)
    .unwrap();
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_ai-token-timeline"))
            .current_dir(&root)
            .arg("--allow-workspace")
            .arg(&root)
            .arg("--config")
            .arg(&config)
            .arg("--db")
            .arg(root.join("usage.sqlite"))
            .arg("compare")
            .args(extra)
            .output()
            .unwrap()
    };
    for count in 0..2 {
        if count == 1 {
            fs::write(all.join("alpha.png"), b"image").unwrap();
        }
        let output = run(&[]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Crie novos perfis"));
    }
    fs::write(all.join("beta.png"), b"image").unwrap();
    let output = run(&[
        "--profile",
        "alpha",
        "--profile",
        "alpha",
        "--prompt",
        "task",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("sem repetições"));
    let output = run(&[
        "--profile",
        "alpha",
        "--profile",
        "unknown",
        "--prompt",
        "task",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("não encontrado"));
    let output = run(&[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Sem terminal interativo"));
    // Permission must stop execution before even compiling these image profiles.
    let output = run(&[
        "--profile",
        "alpha",
        "--profile",
        "beta",
        "--prompt",
        "task",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--allow-tmp"));
    assert!(!root.join(".stackpulse/comparisons").exists());
    let output = run(&[
        "--profile",
        "alpha",
        "--profile",
        "beta",
        "--prompt",
        "task",
        "--allow-tmp",
    ]);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("Use --allow-tmp"));
}
