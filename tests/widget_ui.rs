use ai_token_timeline::{
    analytics::{Metrics, Period, Report, Scope},
    model::Tokens,
    widget,
};
use chrono::Utc;
use std::process::Command;
use unicode_width::UnicodeWidthStr;

fn report(period: Period) -> Report {
    Report {
        period,
        timezone: "UTC".into(),
        from: Utc::now(),
        to: Utc::now(),
        current: Metrics {
            tokens: Tokens {
                input_tokens: 210_000,
                cached_input_tokens: 126_000,
                output_tokens: 42_000,
                reasoning_output_tokens: 12_000,
                ..Default::default()
            },
            total_tokens: 252_000,
            estimated_usd: Some(1.25),
            agents: 21,
            runs: 1,
            active_ms: 120_000,
            agent_ms: 1_320_000,
            ..Default::default()
        },
        previous: Metrics::default(),
        token_change_pct: Some(12.5),
        series: vec![0, 5, 8, 1, 3, 10, 4, 2, 1, 0, 2, 1],
        configurations: Vec::new(),
    }
}

#[test]
fn compact_layout_keeps_all_shortcuts_visible_at_supported_widths() {
    for period in [Period::Hour, Period::Day, Period::Month] {
        for width in [28, 30, 32, 44, 48, 68, 120] {
            let lines = widget::render(&report(period), None, width, "projeto", true);
            assert_eq!(lines.len(), 11);
            assert!(lines.iter().all(|line| line.width() == width.clamp(30, 68)));
            assert!(lines[0].starts_with('╭'));
            assert!(lines[10].ends_with('╯'));
            for key in ['h', 'd', 'm', 'g', 'q'] {
                assert!(lines[9].contains(key), "missing {key} at width {width}");
            }
            assert!(lines[9].contains("q:sair"));
            assert!(lines.iter().all(|line| !line.chars().any(char::is_control)));
        }
    }
}

#[test]
fn wide_layout_retains_usage_cost_time_and_comparison_values() {
    let lines = widget::render(&report(Period::Day), None, 68, "projeto", true);
    let text = lines.join("\n");
    for value in [
        "252.0k",
        "210.0k entrada",
        "42.0k saída",
        "Cache 60%",
        "Raciocínio 12.0k",
        "~US$ 1.2500",
        "2m00s ativo",
        "22m00s agentes",
        "+12.5%",
        "21 agentes",
        "1 exec.",
    ] {
        assert!(text.contains(value), "missing metric {value}");
    }
}

#[test]
fn feedback_empty_state_is_compact_plain_text_and_shows_navigation() {
    for width in [30, 32, 48, 68, 120] {
        let lines = widget::render_feedback(&[], chrono_tz::UTC, &Scope::default(), width).unwrap();
        assert_eq!(lines.len(), 11);
        assert!(lines.iter().all(|line| line.width() == width.clamp(30, 68)));
        assert!(lines.iter().all(|line| !line.chars().any(char::is_control)));
        assert!(lines[9].contains("g:consumo"));
        assert!(lines[9].contains("q:sair"));
    }
}

#[test]
fn widget_capture_and_status_line_emit_no_ansi_when_piped() {
    let dir = tempfile::tempdir().unwrap();
    for (args, expected_lines) in [
        (vec!["--once"], 11),
        (vec!["--once", "--quality"], 11),
        (vec!["--line"], 1),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ai-token-timeline"))
            .current_dir(dir.path())
            .arg("--allow-workspace")
            .arg(dir.path())
            .env("HOME", dir.path())
            .env("TERM", "xterm-256color")
            .env_remove("NO_COLOR")
            .arg("--db")
            .arg(dir.path().join("usage.sqlite"))
            .args(["widget", "--no-sync"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(text.lines().count(), expected_lines);
        assert!(text.chars().all(|c| !c.is_control() || c == '\n'));
    }
}
