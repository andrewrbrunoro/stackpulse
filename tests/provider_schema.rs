use ai_token_timeline::{
    client::Backend,
    profiles::{self, AgentSpec, Profile, Settings, TeamSpec},
    workflow,
};
use chrono::Utc;

fn team() -> TeamSpec {
    serde_json::from_value(serde_json::json!({
        "name": "mixed", "provider": "openai",
        "orchestrator": {"role":"root", "model":"gpt-6-astra", "effort":"medium", "purpose":"Coordinate", "when":"Always"},
        "agents": [{"role":"worker", "model":"grok-4.6", "effort":"high", "purpose":"Implement", "when":"On demand"}],
        "delegation":"on_demand", "integration":"Integrate", "notes":""
    })).unwrap()
}

#[test]
fn legacy_profiles_inherit_provider_without_changing_serialized_shape() {
    let team = team();
    assert_eq!(team.root_provider(), "openai");
    assert_eq!(team.provider_for(&team.agents[0]), "openai");
    assert!(!team.is_mixed());
    assert!(team.orchestrator.provider.is_none());
    let json = serde_json::to_value(&team).unwrap();
    assert!(json["orchestrator"].get("provider").is_none());
    assert!(json["agents"][0].get("provider").is_none());
}

#[test]
fn providers_override_per_role_and_roundtrip_through_profile_markdown() {
    let mut team = team();
    team.orchestrator.provider = Some("anthropic".into());
    team.agents[0].provider = Some("xai".into());
    assert_eq!(team.root_provider(), "anthropic");
    assert_eq!(team.provider_for(&team.agents[0]), "xai");
    assert!(team.is_mixed());
    team.validate_execution_providers().unwrap();
    let profile = Profile {
        schema_version: 1,
        source_image: "mixed.png".into(),
        source_sha256: profiles::digest(b"fixture"),
        generated_by: "test".into(),
        generated_at: Utc::now(),
        team,
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mixed.md");
    let markdown = profiles::markdown(&profile).unwrap();
    assert!(markdown.contains("| worker | xai |"));
    std::fs::write(&path, markdown).unwrap();
    let (loaded, _) = profiles::read(&path).unwrap();
    assert_eq!(loaded.team.root_provider(), "anthropic");
    assert_eq!(loaded.team.provider_for(&loaded.team.agents[0]), "xai");
}

#[test]
fn mixed_profiles_require_supported_roots_and_explicit_child_adapters() {
    let mut team = team();
    team.agents[0].provider = Some("missing_adapter".into());
    assert!(
        team.validate_execution_providers()
            .unwrap_err()
            .to_string()
            .contains("worker")
    );
    team.provider = "xai".into();
    team.agents[0].provider = Some("openai".into());
    assert!(
        team.validate_execution_providers()
            .unwrap_err()
            .to_string()
            .contains("root Codex/Claude")
    );
    team.provider = "openai".into();
    team.agents[0].provider = Some("codex".into());
    assert!(
        !team.is_mixed(),
        "backend aliases must not force bridge execution"
    );
    team.agents[0].provider = Some("invalid/provider".into());
    assert!(team.validate().is_err());
}

#[test]
fn unknown_child_provider_blocks_execution_even_with_a_known_root() {
    let dir = tempfile::tempdir().unwrap();
    let settings = Settings {
        schema_version: 1,
        client: Backend::Codex,
        provider: "openai".into(),
        model: "helper".into(),
        effort: "medium".into(),
        executable: "codex".into(),
        providers_root: dir.path().join("providers"),
        default_profile: None,
        max_agents: 3,
        ai_memory: false,
        ai_usagebar: false,
    };
    let all = settings.providers_root.join("project/all");
    std::fs::create_dir_all(&all).unwrap();
    std::fs::write(all.join("mixed.png"), b"fixture").unwrap();
    let mut team = team();
    team.agents[0].provider = Some("unknown".into());
    let profile = Profile {
        schema_version: 1,
        source_image: "mixed.png".into(),
        source_sha256: profiles::digest(b"fixture"),
        generated_by: "test".into(),
        generated_at: Utc::now(),
        team,
    };
    std::fs::write(all.join("mixed.md"), profiles::markdown(&profile).unwrap()).unwrap();
    assert!(
        profiles::load_for_run(&settings, dir.path(), Some("mixed"))
            .unwrap_err()
            .to_string()
            .contains("unknown")
    );
    // The team's fallback differs from the root and must not select Grok for it.
    let mut team = team_for_root_override();
    let executor = workflow::executor_settings(&settings, &team).unwrap();
    assert_eq!(executor.client, Backend::Codex);
    assert_eq!(executor.provider, "openai");
    assert_eq!(executor.executable, settings.executable);
    team.orchestrator.provider = None;
    assert!(team.validate_execution_providers().is_err());
}

fn team_for_root_override() -> TeamSpec {
    let mut team = team();
    team.provider = "xai".into();
    team.orchestrator.provider = Some("openai".into());
    team.agents[0].provider = Some("anthropic".into());
    team
}

#[test]
fn extraction_schema_accepts_nullable_provider_for_every_role() {
    let schema = profiles::extraction_schema();
    let agent = &schema["properties"]["orchestrator"];
    assert_eq!(
        agent["properties"]["provider"]["type"],
        serde_json::json!(["string", "null"])
    );
    assert!(
        agent["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "provider")
    );
    let mut value = serde_json::to_value(team().orchestrator).unwrap();
    value["provider"] = serde_json::Value::Null;
    let parsed: AgentSpec = serde_json::from_value(value).unwrap();
    assert!(parsed.provider.is_none());
}

#[test]
fn bundled_astra_grok_profile_declares_executable_provider_routes() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("providers/project/all/astra_grok_team.md");
    let (profile, _) = profiles::read(&path).unwrap();
    assert_eq!(profile.team.root_provider(), "openai");
    assert!(profile.team.is_mixed());
    assert_eq!(profile.team.agents.len(), 4);
    assert!(
        profile
            .team
            .agents
            .iter()
            .all(|agent| profile.team.provider_for(agent) == "xai")
    );
    profile.team.validate_execution_providers().unwrap();
    profiles::check_image(&profile, &path).unwrap();
}

#[test]
fn mixed_cursor_children_are_rejected_until_native_delegation_can_be_disabled() {
    let mut team: TeamSpec = serde_json::from_value(serde_json::json!({
        "name":"mixed_cursor", "provider":"openai",
        "orchestrator":{"role":"root", "model":"root-model", "effort":"medium", "purpose":"Coordinate", "when":"Always"},
        "agents":[{"role":"worker", "provider":"cursor", "model":"worker-model", "effort":"high", "purpose":"Implement", "when":"As needed"}],
        "delegation":"on_demand", "integration":"Verify", "notes":""
    })).unwrap();
    assert!(
        team.validate_execution_providers()
            .unwrap_err()
            .to_string()
            .contains("Cursor")
    );
    team.agents[0].provider = Some("xai".into());
    team.validate_execution_providers().unwrap();
}
