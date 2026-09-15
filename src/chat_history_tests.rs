use super::*;
use serde_json::json;

fn open(project: &Path) -> ChatHistory {
    ChatHistory::open(project, &project.join("usage.sqlite")).unwrap()
}
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/history")
        .join(name)
}

#[test]
fn jsonl_persists_incremental_responses_custom_titles_and_interruption() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = open(dir.path());
    let session = history.create_session(dir.path()).unwrap();
    history
        .rename_session(&session.id, dir.path(), "Nova conversa")
        .unwrap();
    let turn = history
        .start_turn(&session.id, "Corrigir busca 界", "time", "Executando")
        .unwrap();
    history
        .update_turn(
            &session.id,
            &turn,
            &["primeira".into(), "parcial".into()],
            "Executando",
            None,
            false,
        )
        .unwrap();
    history
        .update_turn(
            &session.id,
            &turn,
            &["primeira".into(), "resposta completa".into()],
            "Executando",
            None,
            false,
        )
        .unwrap();
    let path = history.path(&session.id).unwrap();
    let size = fs::metadata(&path).unwrap().len();
    history
        .update_turn(
            &session.id,
            &turn,
            &["primeira".into(), "resposta completa".into()],
            "Executando",
            None,
            false,
        )
        .unwrap();
    assert_eq!(
        size,
        fs::metadata(&path).unwrap().len(),
        "unchanged polling must not grow the log"
    );
    let records: Vec<Value> = fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(records.last().unwrap()["keep_lines"], 1);
    assert!(
        history
            .rename_session(&session.id, dir.path(), "\n ")
            .is_err()
    );
    assert!(
        history
            .rename_session(&session.id, dir.path(), &"x".repeat(81))
            .is_err()
    );
    drop(history);
    let mut history = open(dir.path());
    let saved = history.load_session(&session.id, dir.path()).unwrap();
    assert_eq!(saved.summary.title, "Nova conversa");
    assert!(saved.summary.custom_title);
    assert_eq!(saved.turns[0].response, ["primeira", "resposta completa"]);
    assert_eq!(saved.turns[0].status, INTERRUPTED);
    assert!(saved.turns[0].started_at.is_some());
    assert!(saved.turns[0].ended_at.is_some());
}
use serde_json::Value;

#[test]
fn ownership_uses_os_locks_and_releases_after_owner_drops() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = open(dir.path());
    let session = first.create_session(dir.path()).unwrap();
    let mut second = open(dir.path());
    assert!(
        second
            .load_session(&session.id, dir.path())
            .unwrap_err()
            .to_string()
            .contains("outro terminal")
    );
    assert!(
        second
            .rename_session(&session.id, dir.path(), "Intruso")
            .is_err()
    );
    drop(first);
    second.load_session(&session.id, dir.path()).unwrap();
    second
        .rename_session(&session.id, dir.path(), "Recuperado")
        .unwrap();
    second.finish_session(&session.id).unwrap();
    let mut third = open(dir.path());
    assert_eq!(
        third
            .load_session(&session.id, dir.path())
            .unwrap()
            .summary
            .title,
        "Recuperado"
    );
}

#[test]
fn interrupted_final_line_is_backed_up_and_recovered_without_losing_records() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = open(dir.path());
    let session = history.create_session(dir.path()).unwrap();
    let path = history.path(&session.id).unwrap();
    history
        .start_turn(&session.id, "Pedido salvo", "team", "Executando")
        .unwrap();
    drop(history);
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"format\":\"stack")
        .unwrap();
    let mut history = open(dir.path());
    let restored = history.load_session(&session.id, dir.path()).unwrap();
    assert_eq!(restored.turns[0].prompt, "Pedido salvo");
    assert!(history.take_warnings().contains("recuperada"));
    assert!(fs::read_dir(&history.root).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".recovery-")
    }));
}

#[test]
fn invalid_complete_records_and_future_versions_are_not_silently_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = open(dir.path());
    let session = history.create_session(dir.path()).unwrap();
    let path = history.path(&session.id).unwrap();
    drop(history);
    let original = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{original}not json\n")).unwrap();
    let mut history = open(dir.path());
    assert!(history.load_session(&session.id, dir.path()).is_err());
    assert!(fs::read_to_string(&path).unwrap().ends_with("not json\n"));
    fs::write(&path, original.replace("\"version\":1", "\"version\":999")).unwrap();
    assert!(
        history
            .load_session(&session.id, dir.path())
            .unwrap_err()
            .to_string()
            .contains("versão")
    );
}

#[test]
fn project_can_move_without_old_absolute_paths_or_machine_pid_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("old");
    fs::create_dir(&first).unwrap();
    let mut history = open(&first);
    let session = history.create_session(&first).unwrap();
    history
        .start_turn(&session.id, "Trabalho portátil", "team", "Executando")
        .unwrap();
    let identity = history.project_id.clone();
    drop(history);
    let moved = dir.path().join("new");
    fs::rename(first, &moved).unwrap();
    let mut history = open(&moved);
    assert_eq!(history.project_id, identity);
    assert_eq!(
        history.load_session(&session.id, &moved).unwrap().turns[0].prompt,
        "Trabalho portátil"
    );
    let foreign = tempfile::tempdir().unwrap();
    assert!(history.load_session(&session.id, foreign.path()).is_err());
    assert!(history.path("../../secret").is_err());
}

#[test]
fn imports_native_messages_once_keeps_telemetry_and_never_creates_executions() {
    let dir = tempfile::tempdir().unwrap();
    let history = open(dir.path());
    for (file, client, expected_lines) in
        [("codex.jsonl", "codex", 2), ("claude.jsonl", "claude", 2)]
    {
        let source = fixture(file);
        let original = fs::read(&source).unwrap();
        let imported = history.import_session(&source).unwrap();
        assert_eq!(imported.client, client);
        assert!(!imported.duplicate);
        let saved = history.read_session(&imported.id).unwrap();
        assert_eq!(
            saved.turns.len(),
            1,
            "tool results and duplicate Codex events are not new prompts"
        );
        assert_eq!(saved.turns[0].response.len(), expected_lines);
        assert!(saved.turns[0].execution.is_none());
        assert!(saved.turns[0].source_events.len() >= 5);
        assert!(history.import_session(&source).unwrap().duplicate);
        assert_eq!(fs::read(&source).unwrap(), original);
    }
    assert_eq!(history.sessions(dir.path()).unwrap().len(), 2);
}

#[test]
fn import_rejects_mixed_sessions_unknown_formats_and_partial_native_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    let history = open(dir.path());
    let source = dir.path().join("bad.jsonl");
    for data in [
        "{}\n".into(),
        format!(
            "{}\n{{",
            fs::read_to_string(fixture("claude.jsonl")).unwrap()
        ),
        format!(
            "{}\n{}\n",
            fs::read_to_string(fixture("claude.jsonl")).unwrap(),
            json!({"sessionId":"another", "type":"user"})
        ),
    ] {
        fs::write(&source, data).unwrap();
        assert!(history.import_session(&source).is_err());
    }
    assert!(history.sessions(dir.path()).unwrap().is_empty());
}

#[test]
fn export_import_preserves_agent_timing_interventions_and_avoids_turn_collisions() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = open(dir.path());
    let session = history.create_session(dir.path()).unwrap();
    let turn = history
        .start_turn(&session.id, "Revisar API", "team", "Concluído")
        .unwrap();
    let other = history.create_session(dir.path()).unwrap();
    let activity = ActivitySnapshot {
        supported: true,
        omitted: 0,
        agents: vec![crate::activity::AgentActivity {
            id: "child".into(),
            parent_id: Some("root".into()),
            title: "Revisar testes".into(),
            model: Some("modelo-demo".into()),
            effort: Some("high".into()),
            preview: "cargo test".into(),
            status: crate::activity::AgentStatus::Completed,
            started_at: Some(Utc::now() - chrono::Duration::seconds(65)),
            finished_at: Some(Utc::now()),
        }],
    };
    assert!(history.save_activity(&other.id, &turn, &activity).is_err());
    history
        .save_activity(&session.id, &turn, &activity)
        .unwrap();
    let message = AgentMessage {
        id: "request".into(),
        agent_id: "child".into(),
        redirect: true,
        message: "Revisar testes".into(),
        status: "Na fila".into(),
        detail: "Aguardando".into(),
    };
    history
        .record_agent_message(&session.id, &turn, &message)
        .unwrap();
    assert!(
        history
            .record_agent_message(&session.id, &turn, &message)
            .is_err()
    );
    history
        .update_agent_message(&turn, &message.id, "Entregue", "Concluído")
        .unwrap();
    let destination = dir.path().join("export.jsonl");
    history.export_session(&session.id, &destination).unwrap();
    assert!(history.export_session(&session.id, &destination).is_err());
    let imported = history.import_session(&destination).unwrap();
    let saved = history.read_session(&imported.id).unwrap();
    assert_ne!(saved.turns[0].id, turn);
    assert_eq!(
        history.agent_messages(&saved.turns[0].id).unwrap()[0].status,
        "Entregue"
    );
    let restored = history.activity(&saved.turns[0].id).unwrap().unwrap();
    assert_eq!(
        restored.agents[0].elapsed_at(Utc::now()).unwrap().as_secs(),
        65
    );
    let other_machine = tempfile::tempdir().unwrap();
    let migrated = open(other_machine.path());
    let imported = migrated.import_session(&destination).unwrap();
    assert_eq!(
        migrated.read_session(&imported.id).unwrap().turns[0].prompt,
        "Revisar API"
    );
}

#[test]
fn sqlite_migration_preserves_ids_titles_and_turns_once_without_modifying_source() {
    use rusqlite::Connection;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite.chat.sqlite");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE chat_sessions(id TEXT,title TEXT,project TEXT,created_at TEXT,updated_at TEXT,ended_at TEXT,owner_pid INTEGER,custom_title INTEGER);
        CREATE TABLE chat_turns(id TEXT,session_id TEXT,position INTEGER,prompt TEXT,response TEXT,status TEXT,profile TEXT,execution TEXT,started_at TEXT,ended_at TEXT);").unwrap();
    let now = Utc::now().to_rfc3339();
    let project = dir.path().canonicalize().unwrap();
    db.execute(
        "INSERT INTO chat_sessions VALUES ('legacy','Título preservado',?1,?2,?2,?2,NULL,1)",
        rusqlite::params![project.to_string_lossy(), now],
    )
    .unwrap();
    db.execute("INSERT INTO chat_turns VALUES ('turn','legacy',0,'Pedido antigo','[\"Resposta antiga\"]','Concluído','team',NULL,?1,?1)", [&now]).unwrap();
    drop(db);
    let original = fs::read(&path).unwrap();
    let history = open(dir.path());
    let saved = history.read_session("legacy").unwrap();
    assert_eq!(saved.summary.title, "Título preservado");
    assert!(saved.summary.custom_title);
    assert_eq!(saved.turns[0].response, ["Resposta antiga"]);
    assert!(history.take_warnings().contains("1 conversa migrada"));
    drop(history);
    let history = open(dir.path());
    assert_eq!(history.sessions(dir.path()).unwrap().len(), 1);
    assert!(history.take_warnings().is_empty());
    assert_eq!(fs::read(&path).unwrap(), original);
}
