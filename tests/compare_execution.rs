#![cfg(unix)]

use ai_token_timeline::{
    compare::{self, CompareOptions, Delivery},
    db::Db,
    profiles::{self, AgentSpec, Profile, Settings, TeamSpec},
};
use chrono::Utc;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

fn profile(root: &Path, name: &str, model: &str) {
    let directory = root.join("providers/project/all");
    fs::create_dir_all(&directory).unwrap();
    let image = format!("{name}.png");
    fs::write(directory.join(&image), name.as_bytes()).unwrap();
    let profile = Profile {
        schema_version: 1,
        source_image: image,
        source_sha256: profiles::digest(name.as_bytes()),
        generated_by: "test".into(),
        generated_at: Utc::now(),
        team: TeamSpec {
            name: name.into(),
            provider: "openai".into(),
            orchestrator: AgentSpec {
                provider: None,
                role: "root".into(),
                model: model.into(),
                effort: "low".into(),
                purpose: "testar".into(),
                when: "sempre".into(),
            },
            agents: vec![],
            delegation: "on_demand".into(),
            integration: "validar".into(),
            notes: String::new(),
        },
    };
    fs::write(
        directory.join(format!("{name}.md")),
        profiles::markdown(&profile).unwrap(),
    )
    .unwrap();
}

fn fixture() -> (tempfile::TempDir, Settings) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    profile(root, "alpha", "alpha-model");
    profile(root, "beta", "beta-model");
    profile(root, "missing", "missing-model");
    profile(root, "failure", "failure-model");
    let executable = root.join("fake-codex");
    fs::write(
        &executable,
        r#"#!/usr/bin/env python3
import json, os, sys, time
from pathlib import Path
args = sys.argv[1:]
model = args[args.index('--model') + 1]
if Path('enable-barrier').exists() and model in ('alpha-model', 'beta-model'):
    barrier = Path(__file__).with_suffix('.barrier')
    with barrier.open('a') as out:
        out.write(model + '\n')
    deadline = time.time() + 2
    while len(barrier.read_text().splitlines()) < 2 and time.time() < deadline:
        time.sleep(.02)
    if len(barrier.read_text().splitlines()) < 2:
        sys.exit(8)
os.mkdir('exclusive')
Path('delivered.txt').write_text(model)
Path('actual-cwd.txt').write_text(str(Path.cwd()))
Path('target').mkdir(exist_ok=True)
Path('target/artifact.txt').write_text(model)
sys.stdin.read()
print(json.dumps({'type':'thread.started','thread_id':'thread-' + model}))
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'entrega ' + model}}))
if model == 'missing-model':
    print(json.dumps({'type':'turn.completed','usage':{}}))
else:
    input_tokens = 100 if model == 'alpha-model' else 200
    print(json.dumps({'type':'turn.completed','usage':{'input_tokens':input_tokens,'output_tokens':10}}))
if model == 'failure-model':
    sys.exit(7)
"#,
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let settings = Settings {
        schema_version: 1,
        client: Default::default(),
        provider: "openai".into(),
        model: "helper".into(),
        effort: "low".into(),
        executable,
        providers_root: root.join("providers"),
        default_profile: None,
        max_agents: 2,
        ai_memory: false,
        ai_usagebar: false,
    };
    (temporary, settings)
}

fn run(
    root: &Path,
    settings: &Settings,
    names: &[&str],
    test: Option<&str>,
    timeout_secs: u64,
) -> compare::ComparisonReport {
    let mut db = Db::open(&root.join("usage.sqlite")).unwrap();
    let names: Vec<String> = names.iter().map(|name| (*name).into()).collect();
    compare::run(
        &mut db,
        settings,
        CompareOptions {
            cwd: root,
            sessions: &root.join("sessions"),
            profiles: &names,
            prompt: "crie delivered.txt",
            timeout_secs,
            validation_command: test,
        },
    )
    .unwrap()
}

#[test]
fn profiles_run_concurrently_in_persistent_isolated_workspaces() {
    let (temporary, settings) = fixture();
    let root = temporary.path();
    fs::write(root.join("source.txt"), "original").unwrap();
    fs::write(root.join("enable-barrier"), "1").unwrap();
    std::os::unix::fs::symlink(root.join("source.txt"), root.join("source-link.txt")).unwrap();
    let report = run(
        root,
        &settings,
        &["alpha", "beta"],
        Some("test -f delivered.txt"),
        5,
    );

    assert_eq!(report.results.len(), 2);
    assert_eq!(report.winner.as_deref(), Some("alpha"));
    assert_eq!(compare::winner(root).unwrap().as_deref(), Some("alpha"));
    assert!(!root.join("delivered.txt").exists());
    let sessions: std::collections::HashSet<_> = report
        .results
        .iter()
        .filter_map(|result| result.cli_session_id.as_deref())
        .collect();
    assert_eq!(sessions.len(), 2);
    for result in &report.results {
        assert_eq!(result.delivery, Delivery::Validated);
        assert_eq!(result.execution_status, "completed");
        assert!(result.tokens.is_some());
        assert!(result.tokens_complete);
        assert!(result.final_message.contains(&result.profile_name));
        assert!(result.workspace.join("delivered.txt").is_file());
        assert!(result.workspace.join("source.txt").is_file());
        assert_eq!(
            result.workspace,
            report
                .output_dir
                .join(&result.profile_name)
                .join("workspace")
                .join(profiles::project_key(root).unwrap())
        );
        assert_eq!(
            fs::read_to_string(result.workspace.join("actual-cwd.txt")).unwrap(),
            result.workspace.to_string_lossy()
        );
        assert_eq!(
            fs::read_to_string(result.workspace.join("target/artifact.txt")).unwrap(),
            format!("{}-model", result.profile_name)
        );
        assert_eq!(
            fs::read_to_string(result.workspace.join("source-link.txt")).unwrap(),
            "original"
        );
        let local_state = report
            .output_dir
            .join(&result.profile_name)
            .join("state/comparison.sqlite");
        let local_executions = Db::open(&local_state).unwrap().executions().unwrap();
        assert_eq!(local_executions.len(), 1);
        assert_eq!(local_executions[0].profile_name, result.profile_name);
        assert_eq!(
            local_executions[0].project,
            result.workspace.to_string_lossy()
        );
    }
    assert!(report.output_dir.is_absolute());
    assert_eq!(
        report.output_dir.parent().unwrap(),
        std::env::temp_dir().canonicalize().unwrap()
    );
    let directory_name = report.output_dir.file_name().unwrap().to_str().unwrap();
    assert!(directory_name.starts_with("stackpulse-"));
    assert!(directory_name.ends_with("-compare"));
    assert!(report.output_dir.join("report.json").is_file());
    let persisted: compare::ComparisonReport =
        serde_json::from_slice(&fs::read(report.output_dir.join("report.json")).unwrap()).unwrap();
    assert_eq!(persisted.output_dir, report.output_dir);
    assert!(
        !root
            .join(".stackpulse/comparisons")
            .join(&report.id)
            .exists()
    );
    assert_eq!(
        fs::read_dir(root.join(".stackpulse/comparisons"))
            .unwrap()
            .count(),
        1
    );
    let executions = Db::open(&root.join("usage.sqlite"))
        .unwrap()
        .executions()
        .unwrap();
    assert_eq!(executions.len(), 2);
    assert!(executions.iter().all(|execution| {
        execution.prompt_sha256 == profiles::digest(b"crie delivered.txt")
            && execution.prompt_chars == "crie delivered.txt".chars().count()
    }));
    // Delivery paths and internal symlinks remain usable after the source is removed.
    drop(temporary);
    for result in &report.results {
        assert_eq!(
            fs::read_to_string(result.workspace.join("source-link.txt")).unwrap(),
            "original"
        );
    }
    fs::remove_dir_all(&report.output_dir).unwrap();
}

#[test]
fn cli_success_is_never_treated_as_delivery_without_a_test() {
    let (temporary, settings) = fixture();
    let report = run(temporary.path(), &settings, &["missing", "alpha"], None, 5);
    assert!(
        report
            .results
            .iter()
            .all(|result| result.delivery == Delivery::Inconclusive)
    );
    assert!(report.winner.is_none());
    fs::remove_dir_all(&report.output_dir).unwrap();
}

#[test]
fn missing_tokens_prevent_a_winner_and_failed_cli_rejects_delivery() {
    let (temporary, settings) = fixture();
    let root = temporary.path();
    let missing = run(
        root,
        &settings,
        &["alpha", "missing"],
        Some("test -f delivered.txt"),
        5,
    );
    assert!(
        missing
            .results
            .iter()
            .all(|result| result.delivery == Delivery::Validated)
    );
    assert!(missing.results.iter().any(|result| result.tokens.is_none()));
    assert!(missing.winner.is_none());

    let failed = run(
        root,
        &settings,
        &["alpha", "failure"],
        Some("test -f delivered.txt"),
        5,
    );
    let failure = failed
        .results
        .iter()
        .find(|result| result.profile_name == "failure")
        .unwrap();
    assert_eq!(failure.execution_status, "failed");
    assert_eq!(failure.delivery, Delivery::Rejected);
    assert_eq!(
        fs::read_to_string(failure.workspace.join("delivered.txt")).unwrap(),
        "failure-model"
    );
    assert!(failure.workspace.join("target/artifact.txt").is_file());
    assert!(
        failed
            .output_dir
            .join("failure/state/comparison.sqlite")
            .is_file()
    );
    assert_ne!(missing.output_dir, failed.output_dir);
    assert!(missing.output_dir.join("report.json").is_file());
    fs::remove_dir_all(&missing.output_dir).unwrap();
    fs::remove_dir_all(&failed.output_dir).unwrap();
}

#[test]
fn validation_timeout_is_recorded_and_killed() {
    let (temporary, settings) = fixture();
    let report = run(
        temporary.path(),
        &settings,
        &["missing", "failure"],
        Some("sleep 10"),
        1,
    );
    assert!(report.winner.is_none());
    assert!(report.results.iter().all(|result| {
        result.delivery == Delivery::Rejected
            && result
                .validation
                .as_ref()
                .is_some_and(|test| test.timed_out)
            && result.workspace.join("delivered.txt").is_file()
    }));
    fs::remove_dir_all(&report.output_dir).unwrap();
}

#[test]
fn project_scoped_profiles_resolve_in_the_temporary_workspace() {
    let (temporary, settings) = fixture();
    let key = profiles::project_key(temporary.path()).unwrap();
    fs::rename(
        settings.providers_root.join("project/all"),
        settings.providers_root.join("project").join(key),
    )
    .unwrap();
    let report = run(
        temporary.path(),
        &settings,
        &["alpha", "beta"],
        Some("test -f delivered.txt"),
        5,
    );
    assert!(
        report
            .results
            .iter()
            .all(|result| result.delivery == Delivery::Validated)
    );
    fs::remove_dir_all(&report.output_dir).unwrap();
}

#[test]
fn old_reports_without_output_dir_still_resolve_the_winner() {
    let (temporary, settings) = fixture();
    let report = run(
        temporary.path(),
        &settings,
        &["alpha", "beta"],
        Some("test -f delivered.txt"),
        5,
    );
    let mut json = serde_json::to_value(&report).unwrap();
    json.as_object_mut().unwrap().remove("output_dir");
    let old: compare::ComparisonReport = serde_json::from_value(json.clone()).unwrap();
    assert!(old.output_dir.as_os_str().is_empty());
    fs::write(
        temporary.path().join(".stackpulse/comparisons/latest.json"),
        serde_json::to_vec(&json).unwrap(),
    )
    .unwrap();
    assert_eq!(compare::winner(temporary.path()).unwrap(), report.winner);
    fs::remove_dir_all(&report.output_dir).unwrap();
}

#[test]
fn snapshot_errors_preserve_the_output_directory_and_report_its_path() {
    let (temporary, settings) = fixture();
    let external = tempfile::NamedTempFile::new().unwrap();
    std::os::unix::fs::symlink(external.path(), temporary.path().join("outside-link")).unwrap();
    let mut db = Db::open(&temporary.path().join("usage.sqlite")).unwrap();
    let names = vec!["alpha".into(), "beta".into()];
    let error = compare::run(
        &mut db,
        &settings,
        CompareOptions {
            cwd: temporary.path(),
            sessions: &temporary.path().join("sessions"),
            profiles: &names,
            prompt: "crie delivered.txt",
            timeout_secs: 5,
            validation_command: None,
        },
    )
    .unwrap_err();
    let message = error.to_string();
    let directory = Path::new(
        message
            .strip_prefix("Arquivos do compare preservados em ")
            .unwrap(),
    );
    assert!(directory.is_absolute());
    assert!(directory.join("alpha/workspace").is_dir());
    assert!(directory.join("alpha/state").is_dir());
    assert!(db.executions().unwrap().is_empty());
    fs::remove_dir_all(directory).unwrap();
}
