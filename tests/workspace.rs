#![cfg(unix)]

use ai_token_timeline::{
    db::Db,
    profiles::{self, AgentSpec, Profile, Settings, TeamSpec},
};
use chrono::Utc;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

struct Sandbox {
    _temp: tempfile::TempDir,
    root: PathBuf,
    cwd: PathBuf,
    bin: PathBuf,
    db: PathBuf,
    config: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let cwd = root.join("project B/subfolder with spaces");
        let bin = root.join("test-bin");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        for name in ["git", "codex"] {
            executable(
                &bin.join(name),
                ": > \"$STACKPULSE_TEST_EXECUTED\"\nexit 99",
            );
        }
        Self {
            _temp: temp,
            db: root.join("state/database/usage.sqlite"),
            config: root.join("state/config/settings.json"),
            root,
            cwd,
            bin,
        }
    }

    fn command(&self) -> Command {
        self.command_at(
            Path::new(env!("CARGO_BIN_EXE_ai-token-timeline")),
            &self.cwd,
        )
    }

    fn command_at(&self, executable: &Path, cwd: &Path) -> Command {
        let mut command = Command::new(executable);
        command
            .current_dir(cwd)
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("PATH", &self.bin)
            .env("TERM", "xterm-256color")
            .env("STACKPULSE_TEST_EXECUTED", self.root.join("executed"))
            .stdin(Stdio::null())
            .arg("--db")
            .arg(&self.db)
            .arg("--config")
            .arg(&self.config)
            .arg("--sessions")
            .arg(self.root.join("sessions"));
        command
    }

    fn assert_no_access_side_effects(&self) {
        assert!(!self.root.join("executed").exists(), "external CLI started");
        assert!(!self.root.join("state").exists(), "state directory created");
        assert!(!self.cwd.join("providers").exists(), "providers created");
        assert!(!self.root.join("home/.config").exists());
        assert!(!self.root.join("home/.local").exists());
    }
}

fn executable(path: &Path, script: &str) {
    fs::write(path, format!("#!/bin/sh\nset -eu\n{script}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn help_and_version_need_no_workspace_grant_or_access() {
    let sandbox = Sandbox::new();
    for args in [vec!["--help"], vec!["--version"], vec!["run", "--help"]] {
        let output = sandbox.command().args(args).output().unwrap();
        assert_success(&output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("stackpulse"));
        sandbox.assert_no_access_side_effects();
    }
}

#[test]
fn piped_commands_require_a_grant_before_setup_profiles_or_storage() {
    let sandbox = Sandbox::new();
    for args in [
        vec![],
        vec!["setup", "--list-clis", "--json"],
        vec!["setup", "--provider", "openai", "--model", "test"],
        vec!["profiles", "list"],
        vec!["widget", "--once", "--no-sync"],
        vec!["run", "test", "--no-feedback"],
    ] {
        let output = sandbox.command().args(&args).output().unwrap();
        assert!(!output.status.success(), "unexpected grant: {args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("--allow-workspace"),
            "missing actionable consent error: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        sandbox.assert_no_access_side_effects();
    }
}

#[test]
fn parent_child_and_different_directory_grants_are_rejected_before_access() {
    let sandbox = Sandbox::new();
    let child = sandbox.cwd.join("child");
    let sibling = sandbox.root.join("another project");
    fs::create_dir(&child).unwrap();
    fs::create_dir(&sibling).unwrap();
    for allowed in [sandbox.cwd.parent().unwrap(), &child, &sibling] {
        let output = sandbox
            .command()
            .arg("--allow-workspace")
            .arg(allowed)
            .args(["setup", "--provider", "openai", "--model", "test"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "unexpected grant: {allowed:?}");
        sandbox.assert_no_access_side_effects();
    }
}

#[test]
fn exact_relative_and_symlink_grants_are_scoped_to_the_current_invocation() {
    let sandbox = Sandbox::new();
    let alias = sandbox.root.join("workspace alias");
    symlink(&sandbox.cwd, &alias).unwrap();
    for allowed in [&sandbox.cwd, &alias, Path::new(".")] {
        let output = sandbox
            .command()
            .args(["setup", "--list-clis", "--json"])
            .arg("--allow-workspace")
            .arg(allowed)
            .output()
            .unwrap();
        assert_success(&output);
        let inventory: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(inventory["installed"].is_array());
        sandbox.assert_no_access_side_effects();
    }

    // A previous successful invocation does not persist a blanket trust decision.
    let output = sandbox
        .command()
        .args(["setup", "--list-clis", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let output = sandbox
        .command_at(
            Path::new(env!("CARGO_BIN_EXE_ai-token-timeline")),
            sandbox.cwd.parent().unwrap(),
        )
        .arg("--allow-workspace")
        .arg(&sandbox.cwd)
        .args(["setup", "--list-clis", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    sandbox.assert_no_access_side_effects();
}

#[test]
fn approved_setup_resolves_relative_provider_storage_from_the_users_directory() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .arg("--allow-workspace")
        .arg(&sandbox.cwd)
        .args(["setup", "--provider", "openai", "--model", "test"])
        .output()
        .unwrap();
    assert_success(&output);
    let settings = Settings::read(&sandbox.config).unwrap();
    assert_eq!(settings.providers_root, sandbox.cwd.join("providers"));
    assert!(settings.providers_root.join("project/all").is_dir());
    assert!(!sandbox.root.join("executed").exists());
    assert!(!sandbox.db.exists(), "setup should not open the usage DB");
}

#[test]
fn installed_binary_runs_and_records_the_exact_subfolder_approved_by_the_user() {
    let sandbox = Sandbox::new();
    let installation = sandbox.root.join("installation A/bin");
    fs::create_dir_all(&installation).unwrap();
    let installed_binary = installation.join("stackpulse");
    fs::copy(env!("CARGO_BIN_EXE_ai-token-timeline"), &installed_binary).unwrap();
    let capture = sandbox.root.join("capture");
    fs::create_dir(&capture).unwrap();
    let project_root = sandbox.cwd.parent().unwrap();
    executable(
        &sandbox.bin.join("git"),
        "pwd -P > \"$STACKPULSE_TEST_CAPTURE/git-cwd\"\nprintf '%s\\n' \"$STACKPULSE_TEST_PROJECT_ROOT\"",
    );
    executable(
        &sandbox.bin.join("codex"),
        r#"pwd -P > "$STACKPULSE_TEST_CAPTURE/provider-cwd"
printf '%s\n' "$@" > "$STACKPULSE_TEST_CAPTURE/provider-args"
/bin/cat > "$STACKPULSE_TEST_CAPTURE/prompt"
printf 'created in requested workspace\n' > stackpulse-artifact.txt
printf '%s\n' '{"type":"thread.started","thread_id":"workspace-test-session"}' '{"type":"item.completed","item":{"type":"agent_message","text":"Arquivo criado."}}' '{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":20}}'"#,
    );
    let providers = sandbox.root.join("providers");
    write_profile(&providers.join("project/project B"));
    Settings {
        schema_version: 1,
        client: Default::default(),
        provider: "openai".into(),
        model: "test-model".into(),
        effort: "medium".into(),
        executable: sandbox.bin.join("codex"),
        providers_root: providers,
        default_profile: Some("team".into()),
        max_agents: 1,
        ai_memory: false,
        ai_usagebar: false,
    }
    .save(&sandbox.config)
    .unwrap();

    let output = sandbox
        .command_at(&installed_binary, &sandbox.cwd)
        .arg("--allow-workspace")
        .arg(&sandbox.cwd)
        .env("STACKPULSE_TEST_CAPTURE", &capture)
        .env("STACKPULSE_TEST_PROJECT_ROOT", project_root)
        .args([
            "run",
            "Crie stackpulse-artifact.txt nesta pasta",
            "--no-feedback",
            "--timeout",
            "5",
        ])
        .output()
        .unwrap();
    assert_success(&output);
    let expected = sandbox.cwd.to_string_lossy();
    for filename in ["provider-cwd", "git-cwd"] {
        assert_eq!(
            fs::read_to_string(capture.join(filename)).unwrap().trim(),
            expected
        );
    }
    let arguments = fs::read_to_string(capture.join("provider-args")).unwrap();
    let arguments: Vec<_> = arguments.lines().collect();
    let cwd_argument = arguments.iter().position(|arg| *arg == "-C").unwrap() + 1;
    assert_eq!(arguments[cwd_argument], expected);
    assert!(
        !arguments
            .iter()
            .any(|arg| arg.starts_with("--allow-workspace"))
    );
    assert!(sandbox.cwd.join("stackpulse-artifact.txt").is_file());
    assert!(!project_root.join("stackpulse-artifact.txt").exists());
    assert!(!installation.join("stackpulse-artifact.txt").exists());
    let db = Db::open(&sandbox.db).unwrap();
    let executions = db.executions().unwrap();
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].project, expected);
    assert_eq!(executions[0].status, "completed");
    assert_eq!(executions[0].reported_tokens.unwrap().total(), 120);
    assert_eq!(executions[0].profile_name, "team");
}

fn write_profile(directory: &Path) {
    fs::create_dir_all(directory).unwrap();
    fs::write(directory.join("team.png"), b"workspace test image").unwrap();
    let profile = Profile {
        schema_version: 1,
        source_image: "team.png".into(),
        source_sha256: profiles::digest(b"workspace test image"),
        generated_by: "fixture".into(),
        generated_at: Utc::now(),
        team: TeamSpec {
            name: "team".into(),
            provider: "openai".into(),
            orchestrator: AgentSpec {
                provider: None,
                role: "root".into(),
                model: "test-model".into(),
                effort: "medium".into(),
                purpose: "Execute the request in the current directory".into(),
                when: "always".into(),
            },
            agents: Vec::new(),
            delegation: "on_demand".into(),
            integration: "Verify the requested output".into(),
            notes: String::new(),
        },
    };
    fs::write(
        directory.join("team.md"),
        profiles::markdown(&profile).unwrap(),
    )
    .unwrap();
}
