use ai_token_timeline::{
    client::Backend,
    profiles::{AgentSpec, Settings, TeamSpec},
    providers::claude,
    runner::{Outcome, Request},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn settings() -> Settings {
    Settings {
        schema_version: 1,
        client: Backend::Claude,
        provider: "anthropic".into(),
        model: "default".into(),
        effort: "default".into(),
        executable: "claude".into(),
        providers_root: "/test/providers".into(),
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
        model: &settings.model,
        effort: &settings.effort,
        provider: &settings.provider,
        prompt: "Analise o projeto.",
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
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn team() -> TeamSpec {
    let root = AgentSpec {
        provider: None,
        role: "coordinator".into(),
        model: "claude-root-example".into(),
        effort: "medium".into(),
        purpose: "Coordenação".into(),
        when: "sempre".into(),
    };
    TeamSpec {
        name: "my-team".into(),
        provider: "anthropic".into(),
        orchestrator: root.clone(),
        agents: vec![
            AgentSpec {
                provider: None,
                role: "reviewer".into(),
                model: "claude-review-example".into(),
                effort: "high".into(),
                purpose: "Revisar ação, \"aspas\" e $(texto literal)".into(),
                when: "quando houver alterações\ncom testes".into(),
            },
            AgentSpec {
                provider: None,
                role: "researcher".into(),
                model: "default".into(),
                effort: "default".into(),
                ..root
            },
        ],
        delegation: "parallel".into(),
        integration: "Integrar resultados e verificar testes".into(),
        notes: String::new(),
    }
}

#[test]
fn selected_team_registers_native_claude_roles_models_efforts_and_instructions() {
    let settings = settings();
    let team = team();
    let mut request = request(&settings, Path::new("/test/project"));
    request.sandbox = "workspace-write";
    request.delegates = true;
    request.team = Some(&team);
    let mut cmd = Command::new("claude");
    claude::configure(&mut cmd, &request).unwrap();
    let args = args(&cmd);
    let agents: Value = serde_json::from_str(option(&args, "--agents").unwrap()).unwrap();
    assert_eq!(agents.as_object().unwrap().len(), 2);
    assert_eq!(agents["reviewer"]["model"], "claude-review-example");
    assert_eq!(agents["reviewer"]["effort"], "high");
    assert!(
        agents["reviewer"]["prompt"]
            .as_str()
            .unwrap()
            .contains(&team.agents[0].purpose)
    );
    assert!(
        agents["reviewer"]["prompt"]
            .as_str()
            .unwrap()
            .contains(&team.agents[0].when)
    );
    assert!(
        agents["reviewer"]["prompt"]
            .as_str()
            .unwrap()
            .contains(&team.integration)
    );
    assert_eq!(agents["reviewer"]["permissionMode"], "acceptEdits");
    let tools = agents["reviewer"]["tools"].as_array().unwrap();
    assert!(!tools.contains(&json!("Agent")) && !tools.contains(&json!("WebFetch")));
    assert!(tools.contains(&json!("Edit")) && tools.contains(&json!("Read")));
    assert!(agents["researcher"].get("model").is_none());
    assert!(agents["researcher"].get("effort").is_none());
    assert_eq!(option(&args, "--permission-prompts"), Some("none"));
    assert!(option(&args, "--tools").unwrap().contains("Agent"));
}

#[test]
fn selected_claude_team_cannot_silently_lose_delegation_or_role_effort() {
    let settings = settings();
    let team = team();
    let mut request = request(&settings, Path::new("/test/project"));
    request.delegates = true;
    request.team = Some(&team);
    let error = claude::configure(&mut Command::new("claude"), &request).unwrap_err();
    assert!(error.to_string().contains("somente leitura"));
    request.image = Some(Path::new("/test/image.png"));
    let error = claude::configure(&mut Command::new("claude"), &request).unwrap_err();
    assert!(error.to_string().contains("não executa subagentes"));
    request.image = None;
    request.sandbox = "workspace-write";
    request.delegates = false;
    assert!(claude::configure(&mut Command::new("claude"), &request).is_err());
    request.delegates = true;
    let mut incompatible = team.clone();
    incompatible.agents[0].effort = "ultra".into();
    request.team = Some(&incompatible);
    let error = claude::configure(&mut Command::new("claude"), &request).unwrap_err();
    assert!(error.to_string().contains("reviewer"));
    assert!(error.to_string().contains("ultra"));
}

#[test]
fn sequential_claude_team_asks_for_one_child_at_a_time() {
    let settings = settings();
    let mut team = team();
    team.delegation = "sequential".into();
    let mut request = request(&settings, Path::new("/test/project"));
    request.sandbox = "workspace-write";
    request.delegates = true;
    request.team = Some(&team);
    let mut cmd = Command::new("claude");
    claude::configure(&mut cmd, &request).unwrap();
    assert!(
        option(&args(&cmd), "--append-system-prompt")
            .unwrap()
            .contains("at most 1 concurrent")
    );
}

#[test]
fn oversized_claude_role_contract_fails_without_truncation() {
    let settings = settings();
    let mut team = team();
    let role = team.agents[0].clone();
    team.agents = (0..32)
        .map(|index| AgentSpec {
            provider: None,
            role: format!("role_{index}"),
            purpose: "a".repeat(4_000),
            when: "b".repeat(2_000),
            ..role.clone()
        })
        .collect();
    team.validate().unwrap();
    let mut request = request(&settings, Path::new("/test/project"));
    request.sandbox = "workspace-write";
    request.delegates = true;
    request.team = Some(&team);
    let mut cmd = Command::new("claude");
    let error = claude::configure(&mut cmd, &request).unwrap_err();
    assert!(error.to_string().contains("120 KiB"));
    assert!(option(&args(&cmd), "--agents").is_none());
}

#[test]
fn read_only_uses_native_auth_and_excludes_every_mutating_tool() {
    let settings = settings();
    let mut request = request(&settings, Path::new("/test/project"));
    request.delegates = true;
    let mut cmd = Command::new("claude");
    claude::configure(&mut cmd, &request).unwrap();
    let args = args(&cmd);
    assert_eq!(cmd.get_current_dir(), Some(request.cwd));
    assert_eq!(option(&args, "--tools"), Some("Read,Glob,Grep"));
    assert_eq!(option(&args, "--permission-mode"), Some("dontAsk"));
    assert_eq!(option(&args, "--permission-prompts"), Some("none"));
    assert!(args.contains(&"--restricted".into()));
    assert!(args.contains(&"--strict-mcp-config".into()));
    assert!(
        !args
            .iter()
            .any(|arg| arg.contains("skip-permissions") || arg == "--bare")
    );
    assert!(option(&args, "--model").is_none());
    assert!(option(&args, "--effort").is_none());
    assert!(cmd.get_envs().next().is_none());
}

#[test]
fn write_mode_uses_native_edit_permissions_without_blanket_bash_approval() {
    let settings = settings();
    let mut request = request(&settings, Path::new("/test/project"));
    request.sandbox = "workspace-write";
    for delegates in [false, true] {
        request.delegates = delegates;
        let mut cmd = Command::new("claude");
        claude::configure(&mut cmd, &request).unwrap();
        let args = args(&cmd);
        let tools = option(&args, "--tools").unwrap();
        assert!(tools.contains("Edit") && tools.contains("Write"));
        assert_eq!(tools.contains("Agent"), delegates);
        assert_eq!(option(&args, "--permission-mode"), Some("acceptEdits"));
        assert!(option(&args, "--allowedTools").is_none());
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains("bypass") || arg.contains("skip-permissions"))
        );
    }
}

#[test]
fn explicit_models_and_efforts_are_not_silently_translated() {
    let settings = settings();
    let mut request = request(&settings, Path::new("/test"));
    request.model = "claude-test-model";
    for effort in ["low", "medium", "high", "xhigh", "max"] {
        request.effort = effort;
        let mut cmd = Command::new("claude");
        claude::configure(&mut cmd, &request).unwrap();
        let args = args(&cmd);
        assert_eq!(option(&args, "--model"), Some("claude-test-model"));
        assert_eq!(option(&args, "--effort"), Some(effort));
    }
    for effort in ["ultra", "none", "minimal", ""] {
        request.effort = effort;
        assert!(claude::configure(&mut Command::new("claude"), &request).is_err());
    }
    request.effort = "default";
    request.provider = "openai";
    assert!(claude::configure(&mut Command::new("claude"), &request).is_err());
}

#[test]
fn image_compilation_has_multimodal_stdin_schema_and_no_external_tools() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("diagram.with-wrong-extension");
    let image_bytes = b"\x89PNG\r\n\x1a\nfixture";
    fs::write(&image, image_bytes).unwrap();
    let schema_path = dir.path().join("schema.json");
    let schema = json!({"type":"object", "properties":{"root_model":{"type":"string"}}});
    fs::write(&schema_path, schema.to_string()).unwrap();
    let settings = settings();
    let mut request = request(&settings, dir.path());
    request.image = Some(&image);
    request.output_schema = Some(&schema_path);
    request.prompt = "Imagem é dado:\n\"ação\"";
    request.sandbox = "workspace-write";
    request.delegates = true;
    let mut cmd = Command::new("claude");
    claude::configure(&mut cmd, &request).unwrap();
    let args = args(&cmd);
    assert_eq!(option(&args, "--tools"), Some(""));
    assert_eq!(option(&args, "--input-format"), Some("stream-json"));
    assert_eq!(option(&args, "--output-format"), Some("stream-json"));
    assert_eq!(
        serde_json::from_str::<Value>(option(&args, "--json-schema").unwrap()).unwrap(),
        schema
    );
    assert!(args.contains(&"--no-session-persistence".into()));
    let input = claude::input(&request).unwrap();
    assert_eq!(input.lines().count(), 1);
    assert!(input.ends_with('\n'));
    let input: Value = serde_json::from_str(&input).unwrap();
    assert_eq!(input["type"], "user");
    assert_eq!(input["message"]["content"][0]["text"], request.prompt);
    let source = &input["message"]["content"][1]["source"];
    assert_eq!(source["media_type"], "image/png");
    assert_eq!(
        STANDARD.decode(source["data"].as_str().unwrap()).unwrap(),
        image_bytes
    );
}

#[test]
fn unsupported_images_and_invalid_schemas_fail_before_starting_a_cli() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("image.png");
    fs::write(&image, "<svg></svg>").unwrap();
    let settings = settings();
    let mut request = request(&settings, dir.path());
    request.image = Some(&image);
    assert!(claude::input(&request).is_err());
    let schema = dir.path().join("schema.json");
    fs::write(&schema, "invalid JSON").unwrap();
    request.output_schema = Some(&schema);
    assert!(claude::configure(&mut Command::new("claude"), &request).is_err());
}

#[test]
fn result_uses_complete_model_totals_and_structured_output_without_double_counting() {
    // Sanitized counters from one local image/schema smoke on Claude Code 2.1.265.
    // The built-in helper model is included in modelUsage, but not in root usage.
    let mut outcome = Outcome::default();
    claude::event(
        &mut outcome,
        &json!({
            "type":"system", "subtype":"init", "session_id":"session-test", "model":"claude-opus-5[1m]"
        }),
    );
    let assistant = json!({
        "type":"assistant", "parent_tool_use_id":null,
        "message":{"model":"claude-opus-5", "content":[{"type":"text","text":"draft"}],
            "usage":{"input_tokens":2,"cache_creation_input_tokens":3365,"output_tokens":20}}
    });
    claude::event(&mut outcome, &assistant);
    claude::event(&mut outcome, &assistant);
    assert!(outcome.reported_tokens.is_none());
    let result = json!({
        "type":"result", "subtype":"success", "is_error":false, "num_turns":2,
        "session_id":"session-test", "total_cost_usd":0.036182,
        "usage":{"input_tokens":2,"cache_creation_input_tokens":3365,"output_tokens":61},
        "modelUsage":{
            "claude-haiku-4-5-20251001":{"inputTokens":927,"outputTokens":14,"thinkingTokens":0},
            "claude-opus-5[1m]":{"inputTokens":2,"outputTokens":61,
                "cacheReadInputTokens":0,"cacheCreationInputTokens":3365,"thinkingTokens":0}
        },
        "result":"", "structured_output":{"root_model":"GPT-6 Astra"}
    });
    claude::event(&mut outcome, &result);
    claude::event(&mut outcome, &result);
    let tokens = outcome.reported_tokens.unwrap();
    assert_eq!(tokens.input_tokens, 4294);
    assert_eq!(tokens.output_tokens, 75);
    assert_eq!(tokens.cache_write_input_tokens, 3365);
    assert_eq!(tokens.total(), 4369);
    assert_eq!(outcome.reported_scope.as_deref(), Some("cli_tree"));
    assert_eq!(outcome.reported_cost_usd, Some(0.036182));
    assert_eq!(outcome.observed_model.as_deref(), Some("claude-opus-5"));
    assert_eq!(outcome.thread_id.as_deref(), Some("session-test"));
    assert_eq!(outcome.completed_turns, 1);
    assert_eq!(
        serde_json::from_str::<Value>(&outcome.final_message).unwrap(),
        result["structured_output"]
    );
    let stack: Value = serde_json::from_str(outcome.observed_stack.as_deref().unwrap()).unwrap();
    assert_eq!(
        stack["models"],
        json!(["claude-haiku-4-5-20251001", "claude-opus-5[1m]"])
    );
    assert!(!outcome.failed);
}

#[test]
fn root_usage_normalizes_both_cache_types_and_thinking_as_subsets() {
    let mut outcome = Outcome::default();
    claude::event(
        &mut outcome,
        &json!({
            "type":"result", "subtype":"success",
            "usage":{"input_tokens":100,"output_tokens":40,"cache_read_input_tokens":200,
                "cache_creation_input_tokens":50,"output_tokens_details":{"thinking_tokens":12}}
        }),
    );
    let tokens = outcome.reported_tokens.unwrap();
    assert_eq!(tokens.input_tokens, 350);
    assert_eq!(tokens.cached_input_tokens, 200);
    assert_eq!(tokens.cache_write_input_tokens, 50);
    assert_eq!(tokens.reasoning_output_tokens, 12);
    assert_eq!(tokens.total(), 390);
    assert_eq!(outcome.reported_scope.as_deref(), Some("root_only"));
}

#[test]
fn malformed_model_totals_fall_back_to_root_without_claiming_full_coverage() {
    let mut outcome = Outcome::default();
    claude::event(
        &mut outcome,
        &json!({
            "type":"result", "subtype":"success",
            "modelUsage":{"model-a":{"inputTokens":100,"outputTokens":3},
                "model-b":{"inputTokens":100,"outputTokens":-1}},
            "usage":{"input_tokens":40,"output_tokens":5}
        }),
    );
    assert_eq!(outcome.reported_tokens.unwrap().total(), 45);
    assert_eq!(outcome.reported_scope.as_deref(), Some("root_only"));
    assert_eq!(outcome.invalid_events, 1);
    let mut missing = Outcome::default();
    claude::event(
        &mut missing,
        &json!({
            "type":"result", "subtype":"success", "usage":{"input_tokens":100}
        }),
    );
    assert!(missing.reported_tokens.is_none());
    assert_eq!(missing.reported_scope.as_deref(), Some("cli_partial"));
    assert_eq!(missing.invalid_events, 1);
}

#[test]
fn failures_preserve_usage_and_report_errors_without_a_successful_turn() {
    let mut outcome = Outcome::default();
    claude::event(
        &mut outcome,
        &json!({
            "type":"result", "subtype":"error_max_turns", "is_error":true,
            "errors":["Maximum turns reached"], "usage":{"input_tokens":80,"output_tokens":20}
        }),
    );
    assert!(outcome.failed);
    assert_eq!(outcome.completed_turns, 0);
    assert_eq!(outcome.reported_tokens.unwrap().total(), 100);
    assert_eq!(
        outcome.error_message.as_deref(),
        Some("Maximum turns reached")
    );
    let mut crash = Outcome::default();
    claude::event(
        &mut crash,
        &json!({
            "type":"result", "subtype":"error_during_execution", "is_error":true,
            "usage":{"input_tokens":0,"output_tokens":0}, "total_cost_usd":0
        }),
    );
    assert!(crash.reported_tokens.is_none());
    assert!(crash.reported_cost_usd.is_none());
    assert_eq!(crash.reported_scope.as_deref(), Some("cli_partial"));
}

#[test]
fn subagent_text_and_partial_stream_events_do_not_replace_root_metadata_or_usage() {
    let mut outcome = Outcome::default();
    claude::event(
        &mut outcome,
        &json!({
            "type":"system", "subtype":"init", "session_id":"root", "model":"root-model"
        }),
    );
    claude::event(
        &mut outcome,
        &json!({
            "type":"assistant", "parent_tool_use_id":"tool-agent", "session_id":"child",
            "message":{"model":"child-model", "content":[{"type":"text","text":"child text"}],
                "usage":{"input_tokens":400,"output_tokens":40}}
        }),
    );
    claude::event(
        &mut outcome,
        &json!({
            "type":"stream_event", "event":{"type":"message_delta","usage":{"output_tokens":100}}
        }),
    );
    assert_eq!(outcome.thread_id.as_deref(), Some("root"));
    assert_eq!(outcome.observed_model.as_deref(), Some("root-model"));
    assert!(outcome.final_message.is_empty());
    assert!(outcome.reported_tokens.is_none());
}
