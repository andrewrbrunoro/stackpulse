#![cfg(unix)]

use ai_token_timeline::{
    db::Db,
    profiles::{self, AgentSpec, Profile, Settings, TeamSpec},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};

fn fixture(root: &Path) {
    let settings = Settings {
        schema_version: 1,
        client: Default::default(),
        provider: "openai".into(),
        model: "helper".into(),
        effort: "low".into(),
        executable: root.join("fake-codex"),
        providers_root: root.join("providers"),
        default_profile: Some("active-team".into()),
        max_agents: 4,
        ai_memory: false,
        ai_usagebar: false,
    };
    settings.save(&root.join("config.json")).unwrap();
    let all = settings.providers_root.join("project/all");
    fs::create_dir_all(&all).unwrap();
    fs::write(all.join("active-team.png"), b"fixture").unwrap();
    let profile = Profile {
        schema_version: 1,
        source_image: "active-team.png".into(),
        source_sha256: profiles::digest(b"fixture"),
        generated_by: "test".into(),
        generated_at: chrono::Utc::now(),
        team: TeamSpec {
            name: "active-team".into(),
            provider: "openai".into(),
            orchestrator: AgentSpec {
                provider: None,
                role: "root".into(),
                model: "gpt-6-astra".into(),
                effort: "medium".into(),
                purpose: "Continuar a conversa".into(),
                when: "Sempre".into(),
            },
            agents: vec![],
            delegation: "on_demand".into(),
            integration: "Verificar entrega".into(),
            notes: String::new(),
        },
    };
    fs::write(
        all.join("active-team.md"),
        profiles::markdown(&profile).unwrap(),
    )
    .unwrap();
    fs::write(&settings.executable, r#"#!/usr/bin/env python3
import json, sys
from pathlib import Path
Path('provider-input.txt').write_text(sys.stdin.read())
print(json.dumps({'type':'thread.started','thread_id':'chat-fixture'}))
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'Lembrei da conversa.'}}))
print(json.dumps({'type':'turn.completed','usage':{'input_tokens':25,'output_tokens':5}}))
"#).unwrap();
    fs::set_permissions(&settings.executable, fs::Permissions::from_mode(0o700)).unwrap();
}

fn invoke(root: &Path, context: &Path, prompt: &str, preview: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ai-token-timeline"));
    command
        .current_dir(root)
        .arg("--allow-workspace")
        .arg(root)
        .arg("--config")
        .arg(root.join("config.json"))
        .arg("--db")
        .arg(root.join("usage.sqlite"))
        .arg("--sessions")
        .arg(root.join("no-native-sessions"))
        .arg("run")
        .arg("--profile=active-team")
        .arg("--chat-context")
        .arg(context)
        .arg("--no-feedback")
        .arg("--timeout=10");
    if preview {
        command.arg("--dry-run");
    }
    command.arg("--").arg(prompt).output().unwrap()
}

#[test]
fn cli_delivers_history_to_provider_once_and_accounts_for_original_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root);
    let context = root.join("history ' $(literal) `.json");
    let previous = "memória com Unicode 界 e decisões anteriores ".repeat(6000);
    fs::write(&context, serde_json::to_vec(&serde_json::json!({
        "session_id": "only-this-session",
        "turns": [
            {"user":"Defina o nome", "assistant":previous, "profile":"previous-team", "status":"Concluído"},
            {"user":"Acrescente uma regra", "assistant":"Regra azul", "profile":"previous-team", "status":"Cancelado"}
        ]
    })).unwrap()).unwrap();
    let request = "Continue com a decisão azul — pedido único.";
    let result = invoke(root, &context, request, false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let received = fs::read_to_string(root.join("provider-input.txt")).unwrap();
    assert!(received.contains(&previous));
    assert!(received.contains("Regra azul"));
    assert!(received.contains("Cancelado"));
    assert_eq!(received.matches(request).count(), 1);
    assert!(received.ends_with(request));
    assert!(
        received.find("previous-team").unwrap()
            < received.find("PERFIL ATIVO: active-team").unwrap()
    );
    let db = Db::open(&root.join("usage.sqlite")).unwrap();
    let jobs = db.executions().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].prompt_chars, request.chars().count());
    assert_eq!(jobs[0].prompt_sha256, profiles::digest(request.as_bytes()));
    assert_eq!(jobs[0].status, "completed");
    assert_eq!(jobs[0].profile_name, "active-team");

    fs::remove_file(root.join("provider-input.txt")).unwrap();
    let preview = invoke(root, &context, "Prévia atual", true);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    assert!(String::from_utf8_lossy(&preview.stdout).contains(&previous));
    assert!(!root.join("provider-input.txt").exists());
    assert_eq!(db.executions().unwrap().len(), 1);
}

#[test]
fn missing_invalid_and_oversized_context_fail_before_provider_execution() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root);
    let context = root.join("context.json");
    let missing = invoke(root, &context, "Novo pedido", false);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("Histórico indisponível"));
    fs::write(&context, "{truncated").unwrap();
    let invalid = invoke(root, &context, "Novo pedido", false);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("JSON inválido"));
    fs::write(&context, "x".repeat(1_000_001)).unwrap();
    let oversized = invoke(root, &context, "Novo pedido", false);
    assert!(!oversized.status.success());
    assert!(String::from_utf8_lossy(&oversized.stderr).contains("1 MB"));

    // The combined profile, history and request must also fit; never silently
    // drop older turns, even when the context file itself is within the limit.
    fs::write(
        &context,
        serde_json::to_vec(&serde_json::json!({
            "session_id":"large", "turns":[{"user":"old", "assistant":"x".repeat(998_000)}]
        }))
        .unwrap(),
    )
    .unwrap();
    for preview in [false, true] {
        let combined = invoke(root, &context, "new", preview);
        assert!(!combined.status.success());
        assert!(String::from_utf8_lossy(&combined.stderr).contains("1 MB"));
    }
    assert!(!root.join("provider-input.txt").exists());
    assert!(
        Db::open(&root.join("usage.sqlite"))
            .unwrap()
            .executions()
            .unwrap()
            .is_empty()
    );
}
