use ai_token_timeline::{
    client::Backend,
    profiles::{AgentSpec, Settings, TeamSpec},
    providers::agent_cli,
    runner::{Outcome, Request},
};
use serde_json::json;
use std::{fs, path::Path, process::Command};

fn settings(client: Backend, cwd: &Path) -> Settings {
    Settings {
        schema_version: 1,
        client,
        provider: client.default_provider().into(),
        model: "default".into(),
        effort: "default".into(),
        executable: client.command().into(),
        providers_root: cwd.join("providers"),
        default_profile: None,
        max_agents: 4,
        ai_memory: false,
        ai_usagebar: false,
    }
}
fn request<'a>(settings: &'a Settings, cwd: &'a Path) -> Request<'a> {
    Request {
        settings,
        cwd,
        model: "default",
        effort: "default",
        provider: &settings.provider,
        prompt: "Return a result",
        image: None,
        output_schema: None,
        sandbox: "read-only",
        timeout_secs: 30,
        delegates: false,
        team: None,
    }
}
fn args(command: &Command) -> Vec<String> {
    command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}
fn has_pair(args: &[String], key: &str, value: &str) -> bool {
    args.windows(2).any(|pair| pair == [key, value])
}

fn selected_team(provider: &str) -> TeamSpec {
    let root = AgentSpec {
        provider: None,
        role: "coordinator".into(),
        model: "default".into(),
        effort: "default".into(),
        purpose: "Coordenar a entrega".into(),
        when: "sempre".into(),
    };
    TeamSpec {
        name: "selected-team".into(),
        provider: provider.into(),
        orchestrator: root.clone(),
        agents: vec![AgentSpec {
            provider: None,
            role: "reviewer".into(),
            model: "explicit-child-model".into(),
            effort: "high".into(),
            ..root
        }],
        delegation: "parallel".into(),
        integration: "Integrar e verificar".into(),
        notes: String::new(),
    }
}

#[test]
fn cursor_rejects_unbound_selected_roles_before_launching_but_allows_single_agent_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings(Backend::Cursor, dir.path());
    let team = selected_team("cursor");
    let mut req = request(&settings, dir.path());
    req.delegates = true;
    req.team = Some(&team);
    for sandbox in ["read-only", "workspace-write"] {
        req.sandbox = sandbox;
        let error = agent_cli::configure(&mut Command::new("cursor-agent"), &req).unwrap_err();
        assert!(error.to_string().contains("Este adaptador Cursor"));
        assert!(error.to_string().contains("perfil sem subagentes"));
    }
    let mut single = team.clone();
    single.agents.clear();
    req.team = Some(&single);
    req.delegates = false;
    assert!(agent_cli::configure(&mut Command::new("cursor-agent"), &req).is_ok());
}

#[test]
fn grok_rejects_unisolated_selected_roles_without_changing_native_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings(Backend::Grok, dir.path());
    let team = selected_team("xai");
    let mut req = request(&settings, dir.path());
    req.delegates = true;
    req.team = Some(&team);
    for sandbox in ["read-only", "workspace-write"] {
        req.sandbox = sandbox;
        let mut command = Command::new("grok");
        let error = agent_cli::configure(&mut command, &req).unwrap_err();
        assert!(error.to_string().contains("Este adaptador Grok"));
        assert!(error.to_string().contains("configurações locais"));
        assert!(args(&command).is_empty());
    }
    let mut single = team.clone();
    single.agents.clear();
    req.team = Some(&single);
    req.delegates = false;
    let mut command = Command::new("grok");
    agent_cli::configure(&mut command, &req).unwrap();
    assert!(args(&command).contains(&"--no-subagents".into()));
}

#[test]
fn cursor_uses_its_native_read_only_cli_without_bypass_or_forced_models() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings(Backend::Cursor, dir.path());
    let req = request(&settings, dir.path());
    let mut command = Command::new("cursor-agent");
    agent_cli::configure(&mut command, &req).unwrap();
    let args = args(&command);
    assert!(has_pair(&args, "--output-format", "stream-json"));
    assert!(has_pair(&args, "--mode", "ask"));
    assert!(has_pair(&args, "--sandbox", "enabled"));
    assert!(!args.iter().any(|a| matches!(
        a.as_str(),
        "--model" | "--force" | "--yolo" | "--approve-mcps"
    )));
    assert_eq!(command.get_current_dir(), Some(dir.path()));
}

#[test]
fn cursor_preserves_approval_classifier_for_workspace_edits() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings(Backend::Cursor, dir.path());
    let mut req = request(&settings, dir.path());
    req.sandbox = "workspace-write";
    let mut command = Command::new("cursor-agent");
    agent_cli::configure(&mut command, &req).unwrap();
    let args = args(&command);
    assert!(args.contains(&"--auto-review".into()));
    assert!(!args.contains(&"--mode".into()));
    assert!(!args.contains(&"--force".into()));
}

#[test]
fn cursor_effort_requires_explicit_unparameterized_model() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings(Backend::Cursor, dir.path());
    let mut req = request(&settings, dir.path());
    req.effort = "high";
    assert!(agent_cli::configure(&mut Command::new("cursor-agent"), &req).is_err());
    req.model = "claude-opus-4-8";
    let mut command = Command::new("cursor-agent");
    agent_cli::configure(&mut command, &req).unwrap();
    assert!(has_pair(
        &args(&command),
        "--model",
        "claude-opus-4-8[effort=high]"
    ));
    req.effort = "ultra";
    assert!(agent_cli::configure(&mut Command::new("cursor-agent"), &req).is_err());
}

#[test]
fn grok_image_uses_private_json_prompt_file_with_real_acp_attachment() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings(Backend::Grok, dir.path());
    let image = dir.path().join("image.png");
    fs::write(&image, b"\x89PNG\r\n\x1a\nexample").unwrap();
    let mut req = request(&settings, dir.path());
    req.image = Some(&image);
    let file = agent_cli::prompt_file(&req).unwrap().unwrap();
    assert_eq!(file.path().extension().unwrap(), "json");
    let payload: serde_json::Value =
        serde_json::from_slice(&fs::read(file.path()).unwrap()).unwrap();
    assert_eq!(payload[1]["type"], "image");
    assert_eq!(payload[1]["mimeType"], "image/png");
    assert_eq!(payload[1]["data"], "iVBORw0KGgpleGFtcGxl");
    let mut command = Command::new("grok");
    agent_cli::configure_with_input(&mut command, &req, Some(file.path())).unwrap();
    let args = args(&command);
    assert!(has_pair(
        &args,
        "--prompt-file",
        file.path().to_str().unwrap()
    ));
    assert!(has_pair(&args, "--tools", "read_file"));
    assert!(has_pair(&args, "--deny", "MCPTool"));
    assert!(
        !args.iter().any(|a| a.contains("iVBOR")
            || matches!(a.as_str(), "--always-approve" | "--force" | "--yolo"))
    );
    assert!(agent_cli::configure(&mut Command::new("grok"), &req).is_err());
    let path = file.path().to_path_buf();
    drop(file);
    assert!(!path.exists());
}

#[test]
fn grok_permissions_only_allow_explicit_workspace_edits() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings(Backend::Grok, dir.path());
    let mut req = request(&settings, dir.path());
    req.sandbox = "workspace-write";
    let mut command = Command::new("grok");
    agent_cli::configure(&mut command, &req).unwrap();
    let args = args(&command);
    assert!(has_pair(&args, "--permission-mode", "default"));
    let cwd = dir.path().canonicalize().unwrap();
    assert!(has_pair(
        &args,
        "--allow",
        &format!("Edit({}/**)", cwd.display())
    ));
    assert!(has_pair(
        &args,
        "--allow",
        &format!("Write({}/**)", cwd.display())
    ));
    assert!(!has_pair(&args, "--allow", "Bash"));
}

#[test]
fn cursor_terminal_usage_counts_disjoint_cache_buckets_once() {
    let mut outcome = Outcome::default();
    agent_cli::event(
        &mut outcome,
        &json!({"type":"system","subtype":"init","session_id":"cursor-1","model":"Claude Opus"}),
    );
    agent_cli::event(
        &mut outcome,
        &json!({"type":"assistant","message":{"content":[{"type":"text","text":"OK"}],"usage":{"inputTokens":9999,"outputTokens":999}}}),
    );
    assert!(outcome.reported_tokens.is_none());
    let event = json!({"type":"result","subtype":"success","is_error":false,"result":"OK","usage":{"inputTokens":2,"outputTokens":4,"cacheReadTokens":17664,"cacheWriteTokens":8520}});
    agent_cli::event(&mut outcome, &event);
    agent_cli::event(&mut outcome, &event);
    assert_eq!(outcome.reported_tokens.unwrap().total(), 26190);
    assert_eq!(outcome.reported_tokens.unwrap().input_tokens, 26186);
    assert_eq!(outcome.completed_turns, 1);
    assert_eq!(outcome.final_message, "OK");
    assert_eq!(outcome.thread_id.as_deref(), Some("cursor-1"));
    assert_eq!(outcome.observed_model.as_deref(), Some("Claude Opus"));
    assert!(outcome.reported_cost_usd.is_none());
}

#[test]
fn grok_aggregate_replaces_intermediate_usage_and_preserves_model_stack() {
    let mut outcome = Outcome::default();
    agent_cli::event(
        &mut outcome,
        &json!({"type":"usage","usage":{"input_tokens":1000,"output_tokens":100}}),
    );
    assert!(outcome.reported_tokens.is_none());
    agent_cli::event(&mut outcome, &json!({"type":"text","data":"OK"}));
    let end = json!({"type":"end","stopReason":"end_turn","sessionId":"grok-1","usage":{"input_tokens":100,"cache_read_input_tokens":40,"cache_creation_input_tokens":10,"output_tokens":20,"reasoning_tokens":5,"total_tokens":170},"modelUsage":{"grok-4.6-build":{"inputTokens":100}},"total_cost_usd":0.03});
    agent_cli::event(&mut outcome, &end);
    agent_cli::event(&mut outcome, &end);
    assert_eq!(outcome.reported_tokens.unwrap().total(), 170);
    assert_eq!(outcome.completed_turns, 1);
    assert_eq!(outcome.observed_model.as_deref(), Some("grok-4.6-build"));
    assert_eq!(
        outcome.observed_stack.as_deref(),
        Some("[\"grok-4.6-build\"]")
    );
    assert_eq!(outcome.reported_scope.as_deref(), Some("cli_tree"));
    assert_eq!(outcome.reported_cost_usd, Some(0.03));
}

#[test]
fn absent_partial_and_invalid_usage_never_become_free_complete_runs() {
    let mut unknown = Outcome::default();
    agent_cli::event(
        &mut unknown,
        &json!({"type":"end","stopReason":"end_turn","sessionId":"unknown"}),
    );
    assert!(unknown.reported_tokens.is_none());
    assert!(unknown.reported_cost_usd.is_none());
    let mut partial = Outcome::default();
    agent_cli::event(
        &mut partial,
        &json!({"type":"end","stopReason":"end_turn","usage_is_incomplete":true,"cost_is_partial":true,"total_cost_usd":1,"usage":{"input_tokens":100,"output_tokens":10,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}),
    );
    assert_eq!(partial.reported_tokens.unwrap().total(), 110);
    assert_eq!(partial.reported_scope.as_deref(), Some("cli_partial"));
    assert!(partial.reported_cost_usd.is_none());
    let mut invalid = Outcome::default();
    agent_cli::event(
        &mut invalid,
        &json!({"type":"end","stopReason":"end_turn","usage":{"input_tokens":u64::MAX,"cache_read_input_tokens":1,"cache_creation_input_tokens":0,"output_tokens":1}}),
    );
    assert!(invalid.reported_tokens.is_none());
    assert_eq!(invalid.invalid_events, 1);
    let mut missing_cache = Outcome::default();
    agent_cli::event(
        &mut missing_cache,
        &json!({"type":"end","stopReason":"end_turn","usage":{"input_tokens":100,"output_tokens":10}}),
    );
    assert!(missing_cache.reported_tokens.is_none());
}

#[test]
fn failures_and_truncated_runs_are_not_successes_even_with_usage() {
    let mut outcome = Outcome {
        exit_code: Some(0),
        ..Default::default()
    };
    agent_cli::event(
        &mut outcome,
        &json!({"type":"end","stopReason":"max_turn_requests","usage":{"input_tokens":10,"output_tokens":3}}),
    );
    assert!(!outcome.success());
    assert!(
        outcome
            .error_message
            .as_deref()
            .unwrap()
            .contains("max_turn_requests")
    );
    let mut outcome = Outcome::default();
    agent_cli::event(
        &mut outcome,
        &json!({"type":"error","message":"Authentication failed","sessionId":"failed"}),
    );
    assert!(outcome.failed);
    assert_eq!(
        outcome.error_message.as_deref(),
        Some("Authentication failed")
    );
}

#[test]
fn cursor_image_is_explicit_read_tool_context_and_schema_is_in_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings(Backend::Cursor, dir.path());
    let image = dir.path().join("reference.png");
    fs::write(&image, b"\x89PNG\r\n\x1a\nexample").unwrap();
    let schema = dir.path().join("schema.json");
    fs::write(&schema, r#"{"type":"object"}"#).unwrap();
    let mut req = request(&settings, dir.path());
    req.image = Some(&image);
    req.output_schema = Some(&schema);
    let prompt = agent_cli::input(&req).unwrap();
    assert!(prompt.contains("built-in Read tool"));
    assert!(prompt.contains(image.file_name().unwrap().to_str().unwrap()));
    assert!(prompt.contains(r#"{"type":"object"}"#));
    assert!(agent_cli::prompt_file(&req).unwrap().is_none());
}

#[test]
fn grok_full_access_disables_both_permission_prompts_and_sandbox() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings(Backend::Grok, dir.path());
    let mut req = request(&settings, dir.path());
    req.sandbox = "danger-full-access";
    let mut command = Command::new("not-executed");
    agent_cli::configure(&mut command, &req).unwrap();
    let args = args(&command);
    assert!(has_pair(&args, "--permission-mode", "bypassPermissions"));
    assert!(has_pair(&args, "--sandbox", "off"));
    assert!(args.contains(&"--no-subagents".into()));
    assert!(!args.contains(&"--allow".into()));
}
