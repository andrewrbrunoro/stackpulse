#![cfg(unix)]
use ai_token_timeline::{
    db::Db,
    profiles::{self, AgentSpec, Profile, Settings, TeamSpec},
    team_runtime, workflow,
};
use chrono::Utc;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

fn team() -> TeamSpec {
    TeamSpec {
        name: "selected-team".into(),
        provider: "openai".into(),
        orchestrator: AgentSpec {
            role: "root".into(),
            model: "gpt-6-astra".into(),
            effort: "medium".into(),
            purpose: "Coordenar a entrega".into(),
            when: "Sempre".into(),
        },
        agents: vec![AgentSpec {
            role: "researcher".into(),
            model: "gpt-5.6-luna".into(),
            effort: "max".into(),
            purpose: "Analisar confiabilidade".into(),
            when: "Quando houver investigação útil".into(),
        }],
        delegation: "on_demand".into(),
        integration: "Integrar e conferir os resultados".into(),
        notes: "Revisão independente somente quando necessária".into(),
    }
}

#[test]
fn operational_policy_preserves_the_request_and_the_selected_flow() {
    let mut team = team();
    let user = "Avalie arquitetura, UX e testes.\nCrie docs/diagnostico.md.\nNão altere o código.";
    for mode in ["on_demand", "parallel", "sequential"] {
        team.delegation = mode.into();
        let prompt = team_runtime::prompt(&team, 4, user).unwrap();
        assert!(prompt.ends_with(&format!("PEDIDO DO USUÁRIO:\n{user}")));
        assert!(prompt.contains("Não espere que o usuário peça agentes"));
        assert!(prompt.contains(&format!("DELEGAÇÃO AUTOMÁTICA ({mode})")));
        assert!(prompt.contains(&team.agents[0].model));
        assert!(prompt.contains(&team.agents[0].when));
        assert!(prompt.contains(&team.integration));
        if mode == "on_demand" {
            assert!(prompt.contains("Não ative todos os papéis"));
        }
        if mode == "sequential" {
            assert!(prompt.contains("Limite concorrente: 1 subagentes"));
        }
    }
    team.agents.clear();
    let prompt = team_runtime::prompt(&team, 4, user).unwrap();
    assert!(prompt.contains("não crie agentes auxiliares"));
    assert!(prompt.ends_with(user));
}

fn setup(root: &Path) -> Settings {
    Settings {
        schema_version: 1,
        client: Default::default(),
        provider: "openai".into(),
        model: "image-helper-model".into(),
        effort: "low".into(),
        executable: root.join("fake-codex"),
        providers_root: root.join("providers"),
        default_profile: None,
        max_agents: 4,
        ai_memory: false,
        ai_usagebar: false,
    }
}

#[test]
fn selected_profile_reaches_the_cli_as_native_roles_and_all_request_content() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let settings = setup(root);
    let dir = settings.providers_root.join("project/all");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("selected-team.png"), b"fixture").unwrap();
    let mut team = team();
    team.delegation = "sequential".into();
    let profile = Profile {
        schema_version: 1,
        source_image: "selected-team.png".into(),
        source_sha256: profiles::digest(b"fixture"),
        generated_by: "fixture".into(),
        generated_at: Utc::now(),
        team: team.clone(),
    };
    fs::write(
        dir.join("selected-team.md"),
        profiles::markdown(&profile).unwrap(),
    )
    .unwrap();
    // The provider records stdin and reads role files while running. It never
    // invokes an actual model, and verifies the temporary files outlive spawn.
    fs::write(&settings.executable, r#"#!/usr/bin/env python3
import json, sys
from pathlib import Path
args = sys.argv[1:]
overrides = {}
for index, value in enumerate(args[:-1]):
    if value == '-c':
        key, value = args[index + 1].split('=', 1)
        overrides[key] = value
files = {}
for key, value in overrides.items():
    if key.endswith('.config_file'):
        path = json.loads(value)
        files[path] = Path(path).read_text()
Path(__file__).with_suffix('.json').write_text(json.dumps({'args':args, 'prompt':sys.stdin.read(), 'files':files}))
print(json.dumps({'type':'thread.started', 'thread_id':'profile-fixture'}))
print(json.dumps({'type':'item.completed', 'item':{'type':'agent_message','text':'Diagnóstico concluído na fixture.'}}))
print(json.dumps({'type':'turn.completed', 'usage':{'input_tokens':50, 'output_tokens':10}}))
"#).unwrap();
    fs::set_permissions(&settings.executable, fs::Permissions::from_mode(0o700)).unwrap();
    let mut db = Db::open(&root.join("usage.sqlite")).unwrap();
    let user =
        "Avalie arquitetura, experiência no terminal e confiabilidade.\nCrie um diagnóstico curto.";
    let options = || workflow::RunOptions {
        cwd: root,
        sessions: root,
        profile: Some("selected-team"),
        prompt: user,
        benchmark: "profile-test",
        sandbox: "read-only",
        timeout_secs: 10,
        dry_run: false,
        no_feedback: true,
    };
    let job = workflow::run(&mut db, &settings, options())
        .unwrap()
        .unwrap();
    assert_eq!(job.status, "completed");
    assert_eq!(job.planned_stack.name, "selected-team");
    assert_eq!(job.max_agents, 1);
    assert_eq!(settings.max_agents, 4, "saved settings must not change");
    assert_eq!(job.prompt_sha256, profiles::digest(user.as_bytes()));
    assert_eq!(job.prompt_chars, user.chars().count());
    let capture: serde_json::Value =
        serde_json::from_slice(&fs::read(settings.executable.with_extension("json")).unwrap())
            .unwrap();
    let args = capture["args"].as_array().unwrap();
    assert!(args.iter().any(|arg| arg == "gpt-6-astra"));
    assert!(!args.iter().any(|arg| arg == "image-helper-model"));
    assert!(
        args.iter()
            .any(|arg| arg == "agents.max_concurrent_threads_per_session=1")
    );
    assert!(capture["prompt"].as_str().unwrap().ends_with(user));
    let files = capture["files"].as_object().unwrap();
    assert_eq!(files.len(), 1);
    for (path, content) in files {
        let config: toml::Table = content.as_str().unwrap().parse().unwrap();
        assert_eq!(config["model"].as_str(), Some("gpt-5.6-luna"));
        assert_eq!(config["model_reasoning_effort"].as_str(), Some("max"));
        assert!(
            config["developer_instructions"]
                .as_str()
                .unwrap()
                .contains("Analisar confiabilidade")
        );
        assert!(
            !Path::new(path).exists(),
            "role files must be removed after completion"
        );
    }
}
