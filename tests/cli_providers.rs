#![cfg(unix)]

use ai_token_timeline::{
    cli_providers::{discover, resolve_executable, same_executable},
    client::Backend,
    profiles::Settings,
};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn executable(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        "#!/bin/sh\n: > \"$AI_TIMELINE_TEST_MARKER\"\nexit 99\n",
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn isolated_cli(root: &Path) -> Command {
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ai-token-timeline"));
    command
        .current_dir(root)
        .arg("--allow-workspace")
        .arg(root)
        .env_clear()
        .env("HOME", root.join("home"))
        .env("PATH", root.join("bin"))
        .env("AI_TIMELINE_TEST_MARKER", root.join("provider-executed"))
        .stdin(Stdio::null());
    command
}

#[test]
fn catalog_priority_is_independent_of_search_directory_order() {
    let dir = tempfile::tempdir().unwrap();
    let commands = [
        "kiro-cli",
        "goose",
        "droid",
        "amp",
        "aider",
        "opencode",
        "grok",
        "cursor-agent",
        "copilot",
        "gemini",
        "claude",
        "codex",
    ];
    let dirs: Vec<PathBuf> = commands
        .iter()
        .enumerate()
        .map(|(i, command)| {
            let path = dir.path().join(i.to_string());
            executable(&path.join(command));
            path
        })
        .collect();
    let inventory = discover(&dirs);
    let ids: Vec<_> = inventory
        .installed
        .iter()
        .map(|cli| cli.id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "codex", "claude", "gemini", "copilot", "cursor", "grok", "opencode", "aider", "amp",
            "droid", "goose", "kiro",
        ]
    );
    let ready: Vec<_> = inventory
        .installed
        .iter()
        .filter(|cli| cli.adapter_available)
        .map(|cli| cli.id.as_str())
        .collect();
    assert_eq!(ready, ["codex", "claude", "cursor", "grok"]);
}

#[test]
fn discovery_ignores_nonexecutables_directories_and_broken_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("codex"), "plain file").unwrap();
    fs::set_permissions(dir.path().join("codex"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::create_dir(dir.path().join("claude")).unwrap();
    symlink("missing", dir.path().join("gemini")).unwrap();
    assert!(discover(&[dir.path().into()]).installed.is_empty());
}

#[test]
fn symlinks_are_deduplicated_but_distinct_installations_keep_path_priority() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first");
    let alias = dir.path().join("alias");
    let second = dir.path().join("second");
    executable(&first.join("codex"));
    fs::create_dir_all(&alias).unwrap();
    symlink(first.join("codex"), alias.join("codex")).unwrap();
    executable(&second.join("codex"));
    let inventory = discover(&[alias.clone(), first.clone(), second.clone(), alias.clone()]);
    assert_eq!(inventory.installed.len(), 1);
    let cli = &inventory.installed[0];
    assert_eq!(cli.executable, alias.join("codex"));
    assert_eq!(cli.other_installations, [second.join("codex")]);
    assert!(same_executable(&first.join("codex"), &alias.join("codex")));
    assert!(!same_executable(
        &first.join("codex"),
        &second.join("codex")
    ));
}

#[test]
fn grok_agent_alias_is_not_classified_as_cursor_or_counted_twice() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("downloads/grok-1.0.13");
    executable(&target);
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    symlink(&target, bin.join("agent")).unwrap();
    let inventory = discover(std::slice::from_ref(&bin));
    assert_eq!(inventory.installed.len(), 1);
    assert_eq!(inventory.installed[0].id, "grok");
    assert_eq!(inventory.installed[0].executable, bin.join("agent"));

    symlink(&target, bin.join("grok")).unwrap();
    let inventory = discover(std::slice::from_ref(&bin));
    assert_eq!(inventory.installed.len(), 1);
    assert_eq!(inventory.installed[0].id, "grok");
    assert!(inventory.installed[0].other_installations.is_empty());
}

#[test]
fn cursor_agent_alias_is_identified_from_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("cursor-agent/versions/current/run");
    executable(&target);
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    symlink(target, bin.join("agent")).unwrap();
    let inventory = discover(&[bin]);
    assert_eq!(inventory.installed.len(), 1);
    assert_eq!(inventory.installed[0].id, "cursor");
    assert!(inventory.installed[0].adapter_available);
}

#[test]
fn unknown_agent_alias_does_not_assume_a_provider() {
    let dir = tempfile::tempdir().unwrap();
    executable(&dir.path().join("agent"));
    let inventory = discover(&[dir.path().into()]);
    assert_eq!(inventory.installed.len(), 1);
    let cli = &inventory.installed[0];
    assert_eq!(cli.id, "agent-unknown");
    assert!(!cli.adapter_available);
    assert!(
        cli.note
            .as_deref()
            .is_some_and(|note| note.contains("genérico"))
    );
}

#[test]
fn generic_copilot_is_annotated_as_ambiguous() {
    let dir = tempfile::tempdir().unwrap();
    executable(&dir.path().join("copilot"));
    let inventory = discover(&[dir.path().into()]);
    let cli = &inventory.installed[0];
    assert_eq!(cli.id, "copilot");
    assert!(cli.name.contains("confirmar"));
    assert!(cli.note.as_deref().is_some_and(|note| note.contains("AWS")));
    assert!(!cli.adapter_available);
}

#[test]
fn resolution_uses_the_first_executable_and_preserves_explicit_paths() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::write(first.join("codex"), "not executable").unwrap();
    executable(&second.join("codex"));
    let dirs = [first, second.clone()];
    assert_eq!(
        resolve_executable(Path::new("codex"), &dirs),
        Some(second.join("codex"))
    );
    assert_eq!(
        resolve_executable(&second.join("codex"), &[]),
        Some(second.join("codex"))
    );
    assert_eq!(resolve_executable(Path::new("missing"), &dirs), None);
}

#[test]
fn list_clis_json_is_read_only_and_never_starts_detected_executables() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    executable(&root.join("bin/codex"));
    executable(&root.join("home/.local/bin/claude"));
    let config = root.join("config/settings.json");
    let db = root.join("database/usage.sqlite");
    let output = isolated_cli(root)
        .arg("--config")
        .arg(&config)
        .arg("--db")
        .arg(&db)
        .args(["setup", "--list-clis", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let installed = json["installed"].as_array().unwrap();
    for (id, executable) in [
        ("codex", root.join("bin/codex")),
        ("claude", root.join("home/.local/bin/claude")),
    ] {
        let cli = installed.iter().find(|cli| cli["id"] == id).unwrap();
        assert_eq!(cli["executable"], executable.to_str().unwrap());
    }
    assert!(json["searched_directories"].is_array());
    assert!(!config.exists());
    assert!(!config.parent().unwrap().exists());
    assert!(!db.exists());
    assert!(!db.parent().unwrap().exists());
    assert!(!root.join("providers").exists());
    assert!(!root.join("provider-executed").exists());
}

#[test]
fn noninteractive_setup_preserves_a_configured_wrapper_without_running_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let wrapper = root.join("bin/my-codex-wrapper");
    executable(&wrapper);
    executable(&root.join("bin/codex"));
    let config = root.join("config/settings.json");
    let settings = Settings {
        schema_version: 1,
        client: Backend::Codex,
        provider: "openai".into(),
        model: "test-model".into(),
        effort: "medium".into(),
        executable: wrapper.clone(),
        providers_root: root.join("custom-providers"),
        default_profile: Some("custom-team".into()),
        max_agents: 7,
        ai_memory: false,
        ai_usagebar: false,
    };
    settings.save(&config).unwrap();
    let output = isolated_cli(root)
        .arg("--config")
        .arg(&config)
        .args(["setup", "--effort", "high"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let saved = Settings::read(&config).unwrap();
    assert_eq!(saved.executable, wrapper);
    assert_eq!(saved.effort, "high");
    assert_eq!(saved.model, settings.model);
    assert_eq!(saved.providers_root, settings.providers_root);
    assert_eq!(saved.default_profile, settings.default_profile);
    assert_eq!(saved.max_agents, settings.max_agents);
    assert!(!saved.ai_memory);
    assert!(String::from_utf8_lossy(&output.stdout).contains("Executável configurado"));
    assert!(!root.join("provider-executed").exists());
    assert!(
        !root
            .join("home/.local/share/ai-token-timeline/usage.sqlite")
            .exists()
    );
}

#[test]
fn setup_rejects_a_known_unsupported_cli_without_saving() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    executable(&root.join("bin/gemini"));
    let config = root.join("config/settings.json");
    let output = isolated_cli(root)
        .arg("--config")
        .arg(&config)
        .args(["setup", "--executable", "gemini"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("adaptador"));
    assert!(!config.exists());
    assert!(!root.join("providers").exists());
    assert!(!root.join("provider-executed").exists());
}

#[test]
fn setup_connects_supported_installed_clients_using_their_own_defaults() {
    for (command, client) in [
        ("claude", Backend::Claude),
        ("cursor-agent", Backend::Cursor),
        ("grok", Backend::Grok),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let installed = root.join("bin").join(command);
        executable(&installed);
        let config = root.join("config/settings.json");
        let output = isolated_cli(root)
            .arg("--config")
            .arg(&config)
            .args(["setup", "--executable", command])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let saved = Settings::read(&config).unwrap();
        assert_eq!(saved.client, client);
        assert_eq!(saved.provider, client.default_provider());
        assert_eq!(saved.model, client.default_model());
        assert_eq!(saved.effort, client.default_effort());
        assert!(same_executable(&saved.executable, &installed));
        assert!(!root.join("provider-executed").exists());
    }
}

#[test]
fn updating_a_non_codex_wrapper_preserves_the_client_and_edited_model() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let wrapper = root.join("bin/my-claude-wrapper");
    executable(&wrapper);
    let config = root.join("config/settings.json");
    let settings = Settings {
        schema_version: 1,
        client: Backend::Claude,
        provider: "anthropic".into(),
        model: "sonnet".into(),
        effort: "default".into(),
        executable: wrapper.clone(),
        providers_root: root.join("providers"),
        default_profile: None,
        max_agents: 3,
        ai_memory: false,
        ai_usagebar: false,
    };
    settings.save(&config).unwrap();
    let output = isolated_cli(root)
        .arg("--config")
        .arg(&config)
        .args(["setup", "--max-agents", "5"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let saved = Settings::read(&config).unwrap();
    assert_eq!(saved.client, Backend::Claude);
    assert_eq!(saved.model, "sonnet");
    assert_eq!(saved.effort, "default");
    assert_eq!(saved.executable, wrapper);
    assert_eq!(saved.max_agents, 5);
    assert!(!root.join("provider-executed").exists());
}

#[test]
fn inventory_flags_do_not_allow_configuration_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let config = root.join("config/settings.json");
    let output = isolated_cli(root)
        .arg("--config")
        .arg(&config)
        .args(["setup", "--list-clis", "--model", "test-model"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!config.exists());
    assert!(!root.join("providers").exists());
}
