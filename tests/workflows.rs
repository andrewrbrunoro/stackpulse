use ai_token_timeline::{
    db::Db,
    model::Tokens,
    profiles::{self, AgentSpec, Profile, Settings, TeamSpec},
    runner::{self, Outcome},
    trend,
    workflow::{self, Execution, Feedback},
};
use chrono::{DateTime, Duration, Utc};
use std::{fs, path::Path};

fn time() -> DateTime<Utc> {
    "2026-09-12T12:00:00Z".parse().unwrap()
}
fn team() -> TeamSpec {
    TeamSpec {
        name: "team".into(),
        provider: "openai".into(),
        orchestrator: AgentSpec {
            role: "root".into(),
            model: "gpt-6-astra".into(),
            effort: "medium".into(),
            purpose: "Coordenar".into(),
            when: "always".into(),
        },
        agents: vec![AgentSpec {
            role: "worker".into(),
            model: "gpt-5.6-luna".into(),
            effort: "max".into(),
            purpose: "Implementar".into(),
            when: "on demand".into(),
        }],
        delegation: "on_demand".into(),
        integration: "Integrar e testar".into(),
        notes: String::new(),
    }
}
fn settings(path: &Path) -> Settings {
    Settings {
        schema_version: 1,
        client: Default::default(),
        provider: "openai".into(),
        model: "gpt-6-astra".into(),
        effort: "medium".into(),
        executable: "codex".into(),
        providers_root: path.into(),
        default_profile: Some("team".into()),
        max_agents: 4,
        ai_memory: false,
        ai_usagebar: false,
    }
}
fn write_profile(dir: &Path) -> Profile {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("team.png"), b"image fixture").unwrap();
    let p = Profile {
        schema_version: 1,
        source_image: "team.png".into(),
        source_sha256: profiles::digest(b"image fixture"),
        generated_by: "fixture".into(),
        generated_at: time(),
        team: team(),
    };
    fs::write(dir.join("team.md"), profiles::markdown(&p).unwrap()).unwrap();
    p
}
fn execution(id: &str, day: i64, q: Option<f64>) -> Execution {
    Execution {
        id: id.into(),
        kind: "task".into(),
        project: "/test".into(),
        profile_name: "team".into(),
        profile_sha256: "aabbccdd".into(),
        profile_markdown: "snapshot".into(),
        planned_stack: team(),
        observed_stack: Some("observed same stack".into()),
        assistant_provider: "openai".into(),
        assistant_model: "gpt-6-astra".into(),
        assistant_effort: "medium".into(),
        execution_client: Default::default(),
        observed_model: None,
        reported_cost_usd: None,
        max_agents: 4,
        sandbox: "workspace-write".into(),
        prompt_sha256: "hash".into(),
        prompt_chars: 4,
        benchmark: "benchmark-v1".into(),
        started_at: time() + Duration::days(day),
        ended_at: Some(time() + Duration::days(day)),
        wall_ms: Some(1000),
        status: "completed".into(),
        root_id: None,
        cli_session_id: None,
        reported_tokens: Some(Tokens {
            input_tokens: 100,
            output_tokens: 20,
            ..Default::default()
        }),
        metrics: None,
        coverage: "root_only".into(),
        error: None,
        feedback: q.map(|q| Feedback {
            delivered: q,
            speed: 3,
            note: String::new(),
            recorded_at: time(),
        }),
    }
}

#[test]
fn project_overrides_all_and_changes_in_images_are_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("providers");
    let cwd = tmp.path().join("my-project");
    fs::create_dir(&cwd).unwrap();
    write_profile(&root.join("project/all"));
    let s = settings(&root);
    assert_eq!(profiles::resolve(&s, &cwd, None).unwrap().scope, "all");
    fs::create_dir_all(root.join("project/my-project")).unwrap();
    fs::write(root.join("project/my-project/team.png"), b"new image").unwrap();
    let d = profiles::resolve(&s, &cwd, None).unwrap();
    assert_eq!(d.scope, "my-project");
    assert!(!d.compiled);
    let p = write_profile(&root.join("project/my-project"));
    let d = profiles::resolve(&s, &cwd, None).unwrap();
    assert!(d.compiled);
    assert!(profiles::check_image(&p, &d.path).is_ok());
    fs::write(d.path.with_extension("png"), b"edited").unwrap();
    assert!(profiles::check_image(&p, &d.path).is_err());
}

#[test]
fn typed_markdown_roundtrips_and_rejects_duplicate_roles_and_path_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_profile(tmp.path());
    let (read, raw) = profiles::read(&tmp.path().join("team.md")).unwrap();
    assert_eq!(read.team.orchestrator.model, p.team.orchestrator.model);
    assert!(raw.contains("gpt-5.6-luna"));
    let mut bad = team();
    bad.agents.push(bad.orchestrator.clone());
    assert!(bad.validate().is_err());
    assert!(profiles::identifier("../escape").is_err());
    assert!(profiles::identifier("a;touch secret").is_err());
}

#[test]
fn missing_feedback_and_missing_days_are_gaps_and_ewma_is_calculated_correctly() {
    let jobs = vec![
        execution("old", -4, Some(1.0)),
        execution("not-rated", -2, None),
        execution("new", 0, Some(0.0)),
    ];
    let t = trend::daily(&jobs, time(), chrono_tz::UTC, 5, 0.3, None, None).unwrap();
    let p = &t[0].points;
    assert_eq!(p.len(), 5);
    assert_eq!(p[0].smooth_delivery, Some(1.0));
    assert_eq!(p[1].delivery, None);
    assert_eq!(p[2].delivery, None);
    assert_eq!(p[3].smooth_delivery, None);
    assert!((p[4].smooth_delivery.unwrap() - 0.7).abs() < 1e-10);
    assert_eq!(t[0].feedback_count, 2);
    assert_eq!(p[2].tokens, Some(120));
}

#[test]
fn profiles_stacks_and_concurrency_changes_are_not_smoothed_together() {
    let first = execution("a", -1, Some(0.9));
    let mut b = execution("b", 0, Some(0.5));
    b.profile_sha256 = "other".into();
    let mut c = execution("c", 0, Some(0.5));
    c.observed_stack = Some("different-model".into());
    let mut d = execution("d", 0, Some(0.5));
    d.max_agents = 20;
    assert_eq!(
        trend::daily(
            &[first, b, c, d],
            time(),
            chrono_tz::UTC,
            5,
            0.3,
            None,
            None
        )
        .unwrap()
        .len(),
        4
    );
}

#[test]
fn feedback_is_saved_with_immutable_profile_snapshot_and_validated() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = Db::open(&tmp.path().join("db.sqlite")).unwrap();
    let job = execution("job", 0, None);
    db.save_execution(&job).unwrap();
    workflow::save_feedback(
        &mut db,
        "job",
        Feedback {
            delivered: 0.5,
            speed: 2,
            note: "faltou um teste".into(),
            recorded_at: time(),
        },
    )
    .unwrap();
    let saved = db.executions().unwrap();
    assert_eq!(saved[0].profile_markdown, "snapshot");
    assert_eq!(saved[0].feedback.as_ref().unwrap().delivered, 0.5);
    assert!(
        workflow::save_feedback(
            &mut db,
            "job",
            Feedback {
                delivered: 1.2,
                speed: 6,
                note: String::new(),
                recorded_at: time()
            }
        )
        .is_err()
    );
}

#[test]
fn quick_feedback_persists_without_inventing_speed_and_can_be_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("db.sqlite");
    let mut db = Db::open(&path).unwrap();
    db.save_execution(&execution("quick", 0, None)).unwrap();
    for delivered in [1.0, 0.5, 0.0] {
        workflow::save_feedback(
            &mut db,
            "quick",
            Feedback {
                delivered,
                speed: 0,
                note: String::new(),
                recorded_at: time(),
            },
        )
        .unwrap();
        let saved = Db::open(&path).unwrap().executions().unwrap();
        assert_eq!(saved.len(), 1);
        let feedback = saved[0].feedback.as_ref().unwrap();
        assert_eq!(feedback.delivered, delivered);
        assert_eq!(feedback.speed, 0);
        let trends = trend::daily(&saved, time(), chrono_tz::UTC, 5, 0.3, None, None).unwrap();
        let point = trends[0].points.last().unwrap();
        assert_eq!(point.delivery, Some(delivered));
        assert_eq!(point.speed, None);
        assert_eq!(point.smooth_speed, None);
    }
    let mut explicit = execution("explicit", 0, Some(1.0));
    explicit.feedback.as_mut().unwrap().speed = 5;
    db.save_execution(&explicit).unwrap();
    let trends = trend::daily(
        &db.executions().unwrap(),
        time(),
        chrono_tz::UTC,
        5,
        0.3,
        None,
        None,
    )
    .unwrap();
    assert_eq!(trends[0].points.last().unwrap().speed, Some(1.0));
}

#[test]
fn cli_tokens_include_reasoning_only_once_and_usage_is_never_assumed_for_failure() {
    let mut o = Outcome::default();
    o.event(r#"{"type":"thread.started","thread_id":"root"}"#);
    o.event(r#"{"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":600,"output_tokens":200,"reasoning_output_tokens":100}}"#);
    assert_eq!(o.reported_tokens.unwrap().total(), 1200);
    assert_eq!(o.thread_id.as_deref(), Some("root"));
    let mut failed = Outcome::default();
    failed.event(r#"{"type":"turn.failed"}"#);
    assert!(failed.reported_tokens.is_none());
    let mut unknown = Outcome::default();
    unknown.event(r#"{"type":"turn.completed","usage":{}}"#);
    assert!(unknown.reported_tokens.is_none());
}

#[test]
fn benchmark_prompts_are_separate_but_daily_subjective_feedback_can_share_a_line() {
    let a = execution("a", -1, Some(0.9));
    let mut b = execution("b", 0, Some(0.4));
    b.prompt_sha256 = "different".into();
    assert_eq!(
        trend::daily(
            &[a.clone(), b.clone()],
            time(),
            chrono_tz::UTC,
            5,
            0.3,
            None,
            None
        )
        .unwrap()
        .len(),
        2
    );
    let mut a = a;
    a.benchmark.clear();
    b.benchmark.clear();
    assert_eq!(
        trend::daily(&[a, b], time(), chrono_tz::UTC, 5, 0.3, None, None)
            .unwrap()
            .len(),
        1
    );
}

#[cfg(unix)]
fn fake_executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
#[cfg(unix)]
fn end_to_end_runner_records_failure_success_and_does_not_execute_prompt_as_shell() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("providers");
    write_profile(&root.join("project/all"));
    let mut s = settings(&root);
    s.executable = tmp.path().join("fake-codex");
    fake_executable(
        &s.executable,
        "cat >/dev/null\nprintf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"fake-thread\"}' '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":20}}'",
    );
    let mut db = Db::open(&tmp.path().join("db.sqlite")).unwrap();
    let opts = |dry_run| workflow::RunOptions {
        cwd: tmp.path(),
        sessions: Path::new("/absent-test-sessions"),
        profile: Some("team"),
        prompt: "echo $(touch should-not-exist)",
        benchmark: "v1",
        sandbox: "read-only",
        timeout_secs: 2,
        dry_run,
        no_feedback: true,
    };
    assert!(workflow::run(&mut db, &s, opts(true)).unwrap().is_none());
    assert!(db.executions().unwrap().is_empty());
    let j = workflow::run(&mut db, &s, opts(false)).unwrap().unwrap();
    assert_eq!(j.status, "completed");
    assert_eq!(j.reported_tokens.unwrap().total(), 120);
    assert_eq!(j.coverage, "root_only");
    assert!(!tmp.path().join("should-not-exist").exists());
    assert!(j.profile_markdown.contains("gpt-6-astra"));
    fake_executable(&s.executable, "cat >/dev/null\nexit 7");
    let failed = workflow::run(&mut db, &s, opts(false)).unwrap().unwrap();
    assert_eq!(failed.status, "failed");
    assert!(failed.ended_at.is_some());
    assert!(failed.reported_tokens.is_none());
    assert_eq!(db.executions().unwrap().len(), 2);
}

#[test]
#[cfg(unix)]
fn timeout_is_enforced_even_when_stdout_has_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = settings(tmp.path());
    s.executable = tmp.path().join("fake-codex");
    fake_executable(&s.executable, "cat >/dev/null\nexec 1>&-\nexec sleep 10");
    let started = std::time::Instant::now();
    let o = runner::execute(
        runner::Request {
            settings: &s,
            cwd: tmp.path(),
            model: "m",
            effort: "medium",
            provider: "openai",
            prompt: "test",
            image: None,
            output_schema: None,
            sandbox: "read-only",
            timeout_secs: 1,
            delegates: false,
            team: None,
        },
        |_| Ok(()),
    )
    .unwrap();
    assert!(o.timed_out);
    assert!(started.elapsed().as_secs() < 4);
}

#[test]
#[cfg(unix)]
fn image_compiler_writes_valid_markdown_and_separate_compiler_usage() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = settings(tmp.path());
    s.executable = tmp.path().join("fake-codex");
    let final_event = serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":serde_json::to_string(&team()).unwrap()}});
    let script = format!(
        "cat >/dev/null\nprintf '%s\\n' '{}' '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":10,\"output_tokens\":5}}}}'",
        final_event
    );
    fake_executable(&s.executable, &script);
    fs::write(tmp.path().join("team.png"), b"test image").unwrap();
    let md = workflow::compile(&s, &tmp.path().join("team.png"), 2, false).unwrap();
    let (p, _) = profiles::read(&md).unwrap();
    assert_eq!(p.team.name, "team");
    assert_eq!(p.source_sha256, profiles::digest(b"test image"));
    assert!(tmp.path().join("team.compile.jsonl").exists());
    assert!(workflow::compile(&s, &tmp.path().join("team.png"), 2, false).is_err());
}

#[test]
fn legacy_settings_select_codex_and_default_to_optional_memory() {
    let dir = tempfile::tempdir().unwrap();
    let mut json = serde_json::to_value(settings(dir.path())).unwrap();
    json.as_object_mut().unwrap().remove("client");
    json.as_object_mut().unwrap().remove("ai_memory");
    json.as_object_mut().unwrap().remove("ai_usagebar");
    let path = dir.path().join("config.json");
    fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    assert_eq!(
        Settings::read(&path).unwrap().client,
        ai_token_timeline::client::Backend::Codex
    );
    assert!(Settings::read(&path).unwrap().ai_memory);
    assert!(!Settings::read(&path).unwrap().ai_usagebar);
}

#[test]
#[cfg(unix)]
fn each_cli_runs_and_feeds_the_widget_once_with_original_session_provenance() {
    use ai_token_timeline::{
        analytics::{self, Period, Scope},
        client::Backend,
    };
    let fixtures = [
        (
            Backend::Claude,
            serde_json::json!({"type":"result","subtype":"success","session_id":"native-session","result":"OK","usage":{"input_tokens":100,"cache_read_input_tokens":20,"cache_creation_input_tokens":30,"output_tokens":10},"total_cost_usd":0.01}),
        ),
        (
            Backend::Cursor,
            serde_json::json!({"type":"result","subtype":"success","session_id":"native-session","model":"cursor-test","result":"OK","usage":{"inputTokens":100,"cacheReadTokens":20,"cacheWriteTokens":30,"outputTokens":10}}),
        ),
        (
            Backend::Grok,
            serde_json::json!({"type":"end","stopReason":"end_turn","sessionId":"native-session","model":"grok-test","text":"OK","usage":{"input_tokens":100,"cache_read_input_tokens":20,"cache_creation_input_tokens":30,"output_tokens":10,"total_tokens":160},"total_cost_usd":0.01}),
        ),
    ];
    for (client, event) in fixtures {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("providers");
        let dir = root.join("project/all");
        let mut profile = write_profile(&dir);
        profile.team.provider = client.default_provider().into();
        profile.team.orchestrator.model = "default".into();
        profile.team.orchestrator.effort = "default".into();
        profile.team.agents.clear();
        fs::write(dir.join("team.md"), profiles::markdown(&profile).unwrap()).unwrap();
        let mut s = settings(&root);
        s.client = client;
        s.provider = client.default_provider().into();
        s.model = "default".into();
        s.effort = "default".into();
        s.executable = tmp.path().join("fake-cli");
        // Repeated terminal events must replace their cumulative counters.
        fake_executable(
            &s.executable,
            &format!("cat >/dev/null\nprintf '%s\\n' '{}' '{}'", event, event),
        );
        let mut db = Db::open(&tmp.path().join("usage.sqlite")).unwrap();
        let job = workflow::run(
            &mut db,
            &s,
            workflow::RunOptions {
                cwd: tmp.path(),
                sessions: Path::new("/absent-test-sessions"),
                profile: None,
                prompt: "$(touch should-not-exist)",
                benchmark: "cli-regression",
                sandbox: "read-only",
                timeout_secs: 3,
                dry_run: false,
                no_feedback: true,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(job.status, "completed", "{client:?}: {:?}", job.error);
        assert_eq!(job.execution_client, client);
        assert_eq!(job.cli_session_id.as_deref(), Some("native-session"));
        assert_eq!(job.reported_tokens.unwrap().total(), 160);
        let data = db.snapshot().unwrap();
        assert_eq!(data.usage.len(), 1);
        assert_eq!(data.sessions.len(), 1);
        assert_eq!(data.turns.len(), 1);
        assert_eq!(job.root_id.as_ref(), Some(&data.sessions[0].id));
        assert_eq!(
            analytics::report(
                &data,
                &Scope::default(),
                Period::Day,
                Utc::now(),
                chrono_tz::UTC
            )
            .unwrap()
            .current
            .total_tokens,
            160
        );
        let mut refreshed = job.clone();
        workflow::refresh(&mut db, &mut refreshed).unwrap();
        assert_eq!(refreshed.coverage, job.coverage);
        assert_eq!(db.snapshot().unwrap().usage.len(), 1);
        workflow::save_feedback(
            &mut db,
            &job.id,
            Feedback {
                delivered: 1.0,
                speed: 4,
                note: String::new(),
                recorded_at: Utc::now(),
            },
        )
        .unwrap();
        assert_eq!(
            db.snapshot().unwrap().annotations[0].run_id,
            data.sessions[0].id
        );
        assert!(!tmp.path().join("should-not-exist").exists());
    }
}
