use ai_token_timeline::{
    client::Backend,
    profiles::{AgentSpec, Settings, TeamSpec},
    providers::codex,
    runner::Request,
};
use std::{collections::BTreeMap, fs, path::Path, process::Command};

fn settings() -> Settings {
    Settings {
        schema_version: 1,
        client: Backend::Codex,
        provider: "openai".into(),
        model: "gpt-6-astra".into(),
        effort: "medium".into(),
        executable: "not-executed-codex-fixture".into(),
        providers_root: "/test/providers".into(),
        default_profile: None,
        max_agents: 20,
        ai_memory: false,
        ai_usagebar: false,
    }
}

fn team() -> TeamSpec {
    TeamSpec {
        name: "native-team".into(),
        provider: "openai".into(),
        orchestrator: AgentSpec {
            role: "root".into(),
            model: "gpt-6-astra".into(),
            effort: "medium".into(),
            purpose: "Coordenar".into(),
            when: "Sempre".into(),
        },
        agents: [
            ("explorer", "gpt-5.6-luna", "max"),
            ("worker", "gpt-5.6-sol", "high"),
            ("reviewer", "gpt-6-astra", "xhigh"),
        ]
        .into_iter()
        .map(|(role, model, effort)| AgentSpec {
            role: role.into(),
            model: model.into(),
            effort: effort.into(),
            purpose: format!("Responsabilidade de {role}"),
            when: "Quando houver trabalho independente".into(),
        })
        .collect(),
        delegation: "on_demand".into(),
        integration: "Reunir resultados e verificar".into(),
        notes: String::new(),
    }
}

fn request<'a>(settings: &'a Settings, team: Option<&'a TeamSpec>) -> Request<'a> {
    Request {
        settings,
        cwd: Path::new("/test/project with spaces"),
        model: &settings.model,
        effort: &settings.effort,
        provider: &settings.provider,
        prompt: "Pedido original, sem instruções manuais de delegação.",
        image: None,
        output_schema: None,
        sandbox: "read-only",
        timeout_secs: 10,
        delegates: true,
        team,
    }
}

fn args(command: &Command) -> Vec<String> {
    command
        .get_args()
        .map(|v| v.to_string_lossy().into_owned())
        .collect()
}

fn overrides(command: &Command) -> BTreeMap<String, toml::Value> {
    args(command)
        .windows(2)
        .filter(|pair| pair[0] == "-c")
        .map(|pair| {
            let (key, value) = pair[1].split_once('=').unwrap();
            let parsed: toml::Table = format!("value={value}").parse().unwrap();
            (key.into(), parsed["value"].clone())
        })
        .collect()
}

#[test]
fn profile_roles_are_native_capabilities_with_their_own_models_and_living_config_files() {
    let settings = settings();
    let team = team();
    let request = request(&settings, Some(&team));
    let mut command = Command::new(&settings.executable);
    let files = codex::configure(&mut command, &request).unwrap().unwrap();
    let overrides = overrides(&command);
    assert_eq!(overrides["agents.enabled"].as_bool(), Some(true));
    assert_eq!(overrides["features.multi_agent"].as_bool(), Some(true));
    assert_eq!(
        overrides["agents.max_concurrent_threads_per_session"].as_integer(),
        Some(20)
    );
    assert_eq!(overrides["model_reasoning_effort"].as_str(), Some("medium"));
    assert!(!overrides.contains_key("developer_instructions"));
    assert!(!overrides.contains_key("features.multi_agent_v2"));
    for agent in &team.agents {
        let description = overrides[&format!("agents.{}.description", agent.role)]
            .as_str()
            .unwrap();
        assert!(description.contains(&agent.purpose) && description.contains(&agent.when));
        let path = Path::new(
            overrides[&format!("agents.{}.config_file", agent.role)]
                .as_str()
                .unwrap(),
        );
        assert!(path.is_absolute() && path.starts_with(files.path()) && path.is_file());
        let layer: toml::Table = fs::read_to_string(path).unwrap().parse().unwrap();
        assert_eq!(layer["model"].as_str(), Some(agent.model.as_str()));
        assert_eq!(
            layer["model_reasoning_effort"].as_str(),
            Some(agent.effort.as_str())
        );
        assert!(
            layer["developer_instructions"]
                .as_str()
                .unwrap()
                .contains(&agent.purpose)
        );
        assert!(!layer.contains_key("sandbox_mode"));
        assert!(!layer.contains_key("approval_policy"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
        }
    }
    assert_eq!(
        fs::read_dir(files.path()).unwrap().count(),
        team.agents.len()
    );
    let directory = files.path().to_owned();
    drop(files);
    assert!(!directory.exists());
    assert!(
        args(&command)
            .windows(2)
            .any(|p| p == ["--model", "gpt-6-astra"])
    );
    assert!(
        args(&command)
            .windows(2)
            .any(|p| p == ["-C", request.cwd.to_str().unwrap()])
    );
    assert!(command.get_envs().next().is_none());
}

#[test]
fn extraction_disables_delegation_and_default_values_keep_cli_configuration() {
    let mut settings = settings();
    settings.model = "default".into();
    settings.provider = "default".into();
    settings.effort = "default".into();
    let team = team();
    let mut request = request(&settings, Some(&team));
    request.delegates = false;
    request.image = Some(Path::new("/tmp/team image.png"));
    request.output_schema = Some(Path::new("/tmp/schema.json"));
    let mut command = Command::new(&settings.executable);
    assert!(codex::configure(&mut command, &request).unwrap().is_none());
    let overrides = overrides(&command);
    assert_eq!(overrides["agents.enabled"].as_bool(), Some(false));
    assert_eq!(overrides["features.multi_agent"].as_bool(), Some(false));
    assert_eq!(overrides.len(), 2);
    let args = args(&command);
    assert!(!args.iter().any(|v| v == "--model"));
    assert!(
        args.windows(2)
            .any(|p| p == ["--image", "/tmp/team image.png"])
    );
    assert!(
        args.windows(2)
            .any(|p| p == ["--output-schema", "/tmp/schema.json"])
    );
    assert_eq!(args.last().map(String::as_str), Some("-"));
}

#[test]
fn role_metadata_is_serialized_without_turning_profile_text_into_cli_configuration() {
    let settings = settings();
    let mut team = team();
    team.agents.truncate(1);
    team.agents[0].role = "read-helper_1".into();
    team.agents[0].model = "default".into();
    team.agents[0].effort = "default".into();
    team.agents[0].purpose = "Leia \"aspas\"\n[agents]\nenabled = false\n$(touch never-run)".into();
    let mut command = Command::new(&settings.executable);
    let files = codex::configure(&mut command, &request(&settings, Some(&team)))
        .unwrap()
        .unwrap();
    let overrides = overrides(&command);
    assert_eq!(overrides["agents.enabled"].as_bool(), Some(true));
    assert!(
        overrides["agents.read-helper_1.description"]
            .as_str()
            .unwrap()
            .contains(&team.agents[0].purpose)
    );
    let layer: toml::Table = fs::read_to_string(files.path().join("read-helper_1.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(layer.len(), 1);
    assert!(
        layer["developer_instructions"]
            .as_str()
            .unwrap()
            .contains(&team.agents[0].purpose)
    );
}

#[test]
fn reserved_role_names_and_path_traversal_are_rejected_before_creating_roles() {
    let settings = settings();
    for role in [
        "enabled",
        "max_threads",
        "max_concurrent_threads_per_session",
        "../escape",
        "bad.role",
    ] {
        let mut team = team();
        team.agents[0].role = role.into();
        let mut command = Command::new(&settings.executable);
        assert!(
            codex::configure(&mut command, &request(&settings, Some(&team))).is_err(),
            "{role}"
        );
        assert!(
            !overrides(&command)
                .keys()
                .any(|v| v.ends_with(".config_file"))
        );
    }
}

#[test]
fn legacy_requests_and_empty_teams_need_no_temporary_role_files() {
    let settings = settings();
    let mut command = Command::new(&settings.executable);
    assert!(
        codex::configure(&mut command, &request(&settings, None))
            .unwrap()
            .is_none()
    );
    let mut team = team();
    team.agents.clear();
    let mut command = Command::new(&settings.executable);
    assert!(
        codex::configure(&mut command, &request(&settings, Some(&team)))
            .unwrap()
            .is_none()
    );
}
