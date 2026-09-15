use ai_token_timeline::{
    analytics::{self, Period, Scope},
    collector,
    db::Db,
    model::*,
    widget,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use std::{collections::HashSet, io::Cursor};
use unicode_width::UnicodeWidthStr;

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}
fn start() -> DateTime<Utc> {
    at("2026-09-11T12:00:00Z")
}
fn session(id: &str, parent: Option<&str>) -> Session {
    Session {
        id: id.into(),
        parent_id: parent.map(str::to_string),
        name: id.into(),
        project: "/project".into(),
        provider: "openai".into(),
        created_at: start(),
        source: "test".into(),
    }
}
fn usage(id: &str, s: &str, input: u64, output: u64) -> Usage {
    Usage {
        id: id.into(),
        session_id: s.into(),
        turn_id: Some(format!("turn-{s}")),
        at: start() + Duration::seconds(30),
        model: if s == "root" {
            "gpt-6-astra"
        } else {
            "gpt-5.6-luna"
        }
        .into(),
        effort: "ultra".into(),
        service_tier: "unknown".into(),
        tokens: Tokens {
            input_tokens: input,
            output_tokens: output,
            ..Default::default()
        },
    }
}
fn turn(s: &str, from: i64, to: i64) -> Turn {
    Turn {
        id: format!("turn-{s}"),
        session_id: s.into(),
        started_at: start() + Duration::seconds(from),
        ended_at: Some(start() + Duration::seconds(to)),
        status: "completed".into(),
        ttft_ms: Some(20),
    }
}

#[test]
fn twenty_parallel_workers_are_counted_once_and_do_not_multiply_wall_time() {
    let mut data = Dataset::default();
    data.sessions.push(session("root", None));
    data.usage.push(usage("root-usage", "root", 100, 20));
    data.turns.push(turn("root", 0, 120));
    for i in 0..20 {
        let id = format!("worker-{i}");
        data.sessions.push(session(&id, Some("root")));
        data.usage.push(usage(&format!("usage-{i}"), &id, 100, 20));
        data.turns.push(turn(&id, 10, 70));
    }
    let runs = analytics::runs(&data, &Scope::default());
    assert_eq!(runs.len(), 1);
    let r = &runs[0];
    assert_eq!(r.metrics.agents, 21);
    assert_eq!(r.metrics.total_tokens, 2520);
    assert_eq!(r.metrics.active_ms, 120_000);
    assert_eq!(r.metrics.agent_ms, 1_320_000);
    assert!(
        r.configuration
            .contains("20×worker[openai:gpt-5.6-luna:ultra:unknown]")
    );
}

fn log(lines: Vec<serde_json::Value>) -> String {
    lines
        .into_iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}
fn meta(id: &str, time: &str, parent: Option<&str>) -> serde_json::Value {
    json!({"timestamp":time,"type":"session_meta","payload":{"id":id,"timestamp":time,"model_provider":"openai","cwd":"/project","source":parent.map(|p|json!({"subagent":{"thread_spawn":{"parent_thread_id":p}}})).unwrap_or(json!("cli"))}})
}
fn count(time: &str, total: u64, last: u64) -> serde_json::Value {
    json!({"timestamp":time,"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":total,"cached_input_tokens":0,"output_tokens":0},"last_token_usage":{"input_tokens":last,"output_tokens":0}}}})
}

#[test]
fn inherited_parent_history_repeated_counters_and_resets_do_not_inflate_usage() {
    let text = log(vec![
        meta("child", "2026-09-11T12:00:00Z", Some("root")),
        meta("root", "2026-09-11T11:00:00Z", None),
        count("2026-09-11T11:59:00Z", 1000, 1000),
        count("2026-09-11T12:01:00Z", 1100, 100),
        count("2026-09-11T12:01:01Z", 1100, 100),
        count("2026-09-11T12:02:00Z", 50, 50),
    ]);
    let (data, bad, incomplete) = collector::parse(Cursor::new(text)).unwrap();
    assert_eq!((bad, incomplete), (0, 0));
    assert_eq!(data.sessions[0].id, "child");
    assert_eq!(data.usage.len(), 2);
    assert_eq!(
        data.usage.iter().map(|u| u.tokens.total()).sum::<u64>(),
        150
    );
    assert!(data.usage.iter().all(|u| u.session_id == "child"));
}

#[test]
fn first_counter_uses_last_usage_when_inherited_baseline_is_missing() {
    let (data, _, _) = collector::parse(Cursor::new(log(vec![
        meta("a", "2026-09-11T12:00:00Z", None),
        count("2026-09-11T12:01:00Z", 1100, 100),
    ])))
    .unwrap();
    assert_eq!(data.usage[0].tokens.input_tokens, 100);
}

#[test]
fn import_is_idempotent_and_appended_partial_line_is_retried() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(&dir.path().join("test.sqlite")).unwrap();
    let path = dir.path().join("session.jsonl");
    let text = log(vec![
        meta("root", "2026-09-11T12:00:00Z", None),
        count("2026-09-11T12:01:00Z", 100, 100),
    ]);
    std::fs::write(&path, format!("{text}{{\"type\":")).unwrap();
    let first = collector::sync(&mut db, &path).unwrap();
    assert_eq!(first.new_events, 1);
    assert_eq!(first.incomplete_lines, 1);
    std::fs::write(
        &path,
        format!("{}{}\n", text, count("2026-09-11T12:02:00Z", 200, 100)),
    )
    .unwrap();
    assert_eq!(collector::sync(&mut db, &path).unwrap().new_events, 1);
    assert_eq!(collector::sync(&mut db, &path).unwrap().files_unchanged, 1);
    assert_eq!(db.snapshot().unwrap().usage.len(), 2);
}

#[test]
fn cached_and_reasoning_tokens_are_subsets_and_prices_follow_date_and_tier() {
    let tokens = Tokens {
        input_tokens: 1000,
        cached_input_tokens: 600,
        cache_write_input_tokens: 100,
        output_tokens: 200,
        reasoning_output_tokens: 150,
    };
    assert_eq!(tokens.total(), 1200);
    let price = Price {
        provider: "openai".into(),
        model: "gpt-6-astra".into(),
        service_tier: "unknown".into(),
        effective_at: start(),
        input_per_million: 10.0,
        cached_per_million: 1.0,
        cache_write_per_million: 12.5,
        output_per_million: 50.0,
        source: "test only".into(),
    };
    assert!((price.estimate(tokens) - 0.01485).abs() < 1e-9);
    let mut u = usage("u", "root", 0, 0);
    u.tokens = tokens;
    let mut data = Dataset {
        sessions: vec![session("root", None)],
        usage: vec![u],
        prices: vec![price.clone()],
        ..Default::default()
    };
    let ids = HashSet::from(["root".to_string()]);
    let m = analytics::metrics(&data, &ids, start(), start() + Duration::hours(1));
    assert!(m.estimated_usd.is_some());
    let mut later = price.clone();
    later.effective_at = start() + Duration::hours(1);
    later.output_per_million = 100.0;
    data.prices.push(later);
    assert_eq!(
        analytics::metrics(&data, &ids, start(), start() + Duration::hours(1)).estimated_usd,
        m.estimated_usd
    );
    data.usage[0].service_tier = "priority".into();
    assert!(
        analytics::metrics(&data, &ids, start(), start() + Duration::hours(1))
            .estimated_usd
            .is_none()
    );
}

#[test]
fn partial_cost_is_unknown_instead_of_a_misleading_total() {
    let data = Dataset {
        sessions: vec![session("root", None)],
        usage: vec![usage("a", "root", 100, 20)],
        ..Default::default()
    };
    let m = analytics::metrics(
        &data,
        &HashSet::from(["root".into()]),
        start(),
        start() + Duration::hours(1),
    );
    assert_eq!(m.estimated_usd, None);
    assert_eq!(m.priced_events, 0);
}

#[test]
fn calendar_month_and_partial_previous_window_use_local_time() {
    let now = at("2026-09-12T01:30:00Z"); // Still Sep 11 in São Paulo.
    let (from, prev, end) =
        analytics::window(Period::Day, now, chrono_tz::America::Sao_Paulo).unwrap();
    assert_eq!(from, at("2026-09-11T03:00:00Z"));
    assert_eq!(prev, at("2026-09-10T03:00:00Z"));
    assert_eq!(end, at("2026-09-11T01:30:00Z"));
    let (from, prev, end) =
        analytics::window(Period::Month, now, chrono_tz::America::Sao_Paulo).unwrap();
    assert_eq!(from, at("2026-09-01T03:00:00Z"));
    assert_eq!(prev, at("2026-08-01T03:00:00Z"));
    assert_eq!(now - from, end - prev);
    let (from, prev, _) = analytics::window(
        Period::Day,
        at("2026-03-09T03:30:00Z"),
        chrono_tz::America::New_York,
    )
    .unwrap();
    assert_eq!((from - prev).num_hours(), 24);
}

#[test]
fn a_longer_month_does_not_claim_a_comparable_percentage() {
    let mut data = benchmark_data(1, 0.8);
    data.usage[0].at = at("2026-02-15T12:00:00Z");
    data.usage[1].at = at("2026-03-15T12:00:00Z");
    let r = analytics::report(
        &data,
        &Scope::default(),
        Period::Month,
        at("2026-03-31T12:00:00Z"),
        chrono_tz::UTC,
    )
    .unwrap();
    assert!(r.current.total_tokens > 0 && r.previous.total_tokens > 0);
    assert_eq!(r.token_change_pct, None);
}

#[test]
fn intervals_are_clipped_and_stale_open_turns_do_not_accumulate_forever() {
    let mut t = turn("root", 0, 120);
    t.ended_at = None;
    let data = Dataset {
        sessions: vec![session("root", None)],
        usage: vec![usage("u", "root", 100, 0)],
        turns: vec![t],
        ..Default::default()
    };
    let m = analytics::metrics(
        &data,
        &HashSet::from(["root".into()]),
        start() + Duration::seconds(10),
        start() + Duration::days(5),
    );
    assert_eq!(m.active_ms, 20_000);
    assert_eq!(m.open_turns, 1);
}

#[test]
fn resuming_a_session_does_not_extend_an_old_unclosed_turn_across_idle_time() {
    let mut first = turn("root", 0, 60);
    first.ended_at = None;
    let mut second = turn("root", 3600, 3660);
    second.id = "second-turn".into();
    let mut second_usage = usage("second-usage", "root", 100, 0);
    second_usage.turn_id = Some(second.id.clone());
    second_usage.at = start() + Duration::seconds(3640);
    let data = Dataset {
        sessions: vec![session("root", None)],
        usage: vec![usage("first-usage", "root", 100, 0), second_usage],
        turns: vec![first, second],
        ..Default::default()
    };
    let m = analytics::metrics(
        &data,
        &HashSet::from(["root".into()]),
        start(),
        start() + Duration::hours(2),
    );
    assert_eq!(m.active_ms, 90_000);
    assert_eq!(m.agent_ms, 90_000);
}

fn benchmark_data(n: usize, quality: f64) -> Dataset {
    let mut data = Dataset::default();
    for i in 0..n * 2 {
        let id = format!("r{i}");
        let mut u = usage(&format!("u{i}"), &id, 100, 20);
        u.model = "same-model".into();
        data.sessions.push(session(&id, None));
        data.usage.push(u);
        data.turns.push(turn(&id, 0, 60));
        data.annotations.push(Annotation {
            run_id: id,
            label: String::new(),
            benchmark: "suite@v1|prompt@1|concurrency=1".into(),
            quality: Some(if i < n { 0.9 } else { quality }),
            baseline: i < n,
        });
    }
    data
}

#[test]
fn quality_decline_requires_repeated_matching_benchmarks() {
    let data = benchmark_data(5, 0.6);
    let c = analytics::comparisons(&analytics::runs(&data, &Scope::default()));
    assert_eq!(c.len(), 1);
    assert!(c[0].regression_signal);
    assert!((c[0].quality_change_pp.unwrap() + 30.0).abs() < 1e-8);
    let small =
        analytics::comparisons(&analytics::runs(&benchmark_data(2, 0.6), &Scope::default()));
    assert!(!small[0].regression_signal);
    assert!(small[0].quality_ci95_pp.is_none());
    let stable =
        analytics::comparisons(&analytics::runs(&benchmark_data(5, 0.9), &Scope::default()));
    assert!(!stable[0].regression_signal);
}

#[test]
fn different_effort_is_not_compared_as_provider_regression() {
    let mut data = benchmark_data(5, 0.6);
    for u in &mut data.usage[5..] {
        u.effort = "low".into();
    }
    let c = analytics::comparisons(&analytics::runs(&data, &Scope::default()));
    assert_eq!(c.len(), 2);
    assert!(c.iter().all(|x| x.quality_change_pp.is_none()));
}

#[test]
fn widget_is_compact_and_never_emits_controls_from_user_labels() {
    let report = analytics::report(
        &benchmark_data(1, 0.8),
        &Scope::default(),
        Period::Day,
        start() + Duration::hours(1),
        chrono_tz::UTC,
    )
    .unwrap();
    let lines = widget::render(&report, None, 68, "\x1b[31mtest\nattack", true);
    assert_eq!(lines.len(), 11);
    assert!(lines.iter().all(|s| s.width() == 68));
    assert!(lines.iter().all(|s| !s.chars().any(char::is_control)));
    assert!(widget::one_line(&report).contains("$?"));
}

#[test]
fn invalid_import_rolls_back_without_losing_saved_events() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(&dir.path().join("db.sqlite")).unwrap();
    let data = benchmark_data(1, 0.8);
    db.ingest(&data).unwrap();
    let mut bad = data.clone();
    bad.usage[0].tokens.cached_input_tokens = 10000;
    assert!(db.ingest(&bad).is_err());
    assert_eq!(db.snapshot().unwrap().usage.len(), 2);
    let mut bad = data;
    bad.annotations[0].quality = Some(1.1);
    assert!(db.ingest(&bad).is_err());
}
