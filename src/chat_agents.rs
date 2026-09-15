//! Compact view of observed subagents, with stable IDs for opening their details.
use crate::{
    activity::{ActivitySnapshot, AgentActivity, AgentStatus},
    chat_tabs::Rect,
    tui::{Frame, Tone},
};
use chrono::Utc;
use unicode_width::UnicodeWidthChar;

const CARD_HEIGHT: usize = 7;

pub(crate) fn width(columns: usize) -> Option<usize> {
    (columns >= 110).then(|| (columns / 4).clamp(36, 54))
}

pub(crate) fn draw(
    frame: &mut Frame,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    snapshot: &ActivitySnapshot,
    offset: &mut usize,
) -> Vec<(Rect, String)> {
    let mut hits = Vec::new();
    let width = width.min(frame.width.saturating_sub(x));
    let height = height.min(frame.height.saturating_sub(y));
    if width == 0 || height == 0 {
        return hits;
    }
    let active = snapshot
        .agents
        .iter()
        .filter(|agent| {
            matches!(
                agent.status,
                AgentStatus::Starting | AgentStatus::Running | AgentStatus::Waiting
            )
        })
        .count();
    put(
        frame,
        x,
        y,
        width,
        &format!(
            "SUBAGENTES · {active} {} · {} total",
            if active == 1 { "ativo" } else { "ativos" },
            snapshot.agents.len()
        ),
        Tone::Accent,
    );
    if snapshot.agents.is_empty() {
        *offset = 0;
        let (headline, explanation) = if snapshot.supported {
            (
                "Nenhum subagente em atividade.",
                "As tarefas delegadas aparecerão aqui com status e progresso.",
            )
        } else {
            (
                "Atividade de agentes indisponível.",
                "Este CLI ainda não informou subagentes.",
            )
        };
        if height > 2 {
            put(frame, x, y + 2, width, headline, Tone::Navigation);
        }
        if height > 3 {
            for (row, text) in lines(explanation, width, height - 3).iter().enumerate() {
                put(frame, x, y + 3 + row, width, text, Tone::Muted);
            }
        }
        return hits;
    }

    // Reserve a header, a gap and a footer. Scrolling counts cards, not lines.
    let capacity = (height.saturating_sub(3) + 1) / (CARD_HEIGHT + 1);
    let shown = if width >= 8 { capacity.max(1) } else { 1 }.min(snapshot.agents.len());
    *offset = (*offset).min(snapshot.agents.len().saturating_sub(shown));
    if capacity > 0 && width >= 8 {
        for (index, agent) in snapshot.agents.iter().skip(*offset).take(shown).enumerate() {
            let card_y = y + 2 + index * (CARD_HEIGHT + 1);
            card(frame, x, card_y, width, agent);
            hits.push((
                Rect {
                    x,
                    y: card_y,
                    width,
                    height: CARD_HEIGHT,
                },
                agent.id.clone(),
            ));
        }
    } else if height >= 6 {
        compact(
            frame,
            x,
            y + 1,
            width,
            height - 2,
            &snapshot.agents[*offset],
        );
        hits.push((
            Rect {
                x,
                y: y + 1,
                width,
                height: height - 2,
            },
            snapshot.agents[*offset].id.clone(),
        ));
    } else if height > 2 {
        put(
            frame,
            x,
            y + 1,
            width,
            "Amplie para ver os detalhes.",
            Tone::Muted,
        );
    }
    if height > 1 {
        let footer = format!(
            "{}–{} / {} · Alt+↑↓{}",
            *offset + 1,
            *offset + shown,
            snapshot.agents.len(),
            if snapshot.omitted > 0 {
                format!(" · +{} omitidos", snapshot.omitted)
            } else {
                String::new()
            }
        );
        put(frame, x, y + height - 1, width, &footer, Tone::Muted);
    }
    hits
}

fn card(frame: &mut Frame, x: usize, y: usize, width: usize, agent: &AgentActivity) {
    let (status, tone) = status(&agent.status);
    put(
        frame,
        x,
        y,
        width,
        &format!("╭{}╮", "─".repeat(width - 2)),
        Tone::Muted,
    );
    status_with_time(frame, x + 2, y, width - 4, status, tone, agent);
    for row in 1..CARD_HEIGHT - 1 {
        put(frame, x, y + row, 1, "│", Tone::Muted);
        put(frame, x + width - 1, y + row, 1, "│", Tone::Muted);
    }
    put(
        frame,
        x,
        y + CARD_HEIGHT - 1,
        width,
        &format!("╰{}╯", "─".repeat(width - 2)),
        Tone::Muted,
    );
    if width >= 16 {
        put(
            frame,
            x + width - 11,
            y + CARD_HEIGHT - 1,
            9,
            " Abrir › ",
            Tone::Navigation,
        );
    }
    let inner = width - 4;
    for (row, text) in lines(&title(agent), inner, 2).iter().enumerate() {
        put(frame, x + 2, y + 1 + row, inner, text, Tone::Accent);
    }
    put(
        frame,
        x + 2,
        y + 3,
        inner,
        &format!("ID · {}", identity(agent)),
        Tone::Muted,
    );
    put(
        frame,
        x + 2,
        y + 4,
        inner,
        &format!("Modelo · {}", model(agent)),
        Tone::Muted,
    );
    put(
        frame,
        x + 2,
        y + 5,
        inner,
        &format!("↳ {}", preview(agent)),
        if agent.preview.trim().is_empty() {
            Tone::Muted
        } else {
            Tone::Text
        },
    );
}

fn compact(
    frame: &mut Frame,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    agent: &AgentActivity,
) {
    let (status, tone) = status(&agent.status);
    status_with_time(frame, x, y, width, status, tone, agent);
    let title_rows = if height >= 6 { 2 } else { 1 };
    for (row, text) in lines(&title(agent), width, title_rows).iter().enumerate() {
        put(frame, x, y + 1 + row, width, text, Tone::Accent);
    }
    put(
        frame,
        x,
        y + 1 + title_rows,
        width,
        &format!("Modelo · {}", model(agent)),
        Tone::Muted,
    );
    for (row, text) in lines(
        &format!("↳ {}", preview(agent)),
        width,
        height - 2 - title_rows,
    )
    .iter()
    .enumerate()
    {
        put(frame, x, y + 2 + title_rows + row, width, text, Tone::Text);
    }
}

fn status_with_time(
    frame: &mut Frame,
    x: usize,
    y: usize,
    width: usize,
    status: &str,
    tone: Tone,
    agent: &AgentActivity,
) {
    let timer = format!(" {} ", elapsed(agent));
    let timer_width = timer.len();
    let show_timer = width >= timer_width + 5;
    let status_width = if show_timer {
        width - timer_width - 1
    } else {
        width
    };
    put(frame, x, y, status_width, &format!(" {status} "), tone);
    if show_timer {
        // Numeric text is generated locally; retain padding over the border.
        frame.put(x + width - timer_width, y, &timer, Tone::Muted);
    }
}

pub(crate) fn elapsed(agent: &AgentActivity) -> String {
    agent.elapsed_at(Utc::now()).map_or_else(
        || "--:--".into(),
        |duration| {
            let seconds = duration.as_secs();
            if seconds >= 86_400 {
                format!(
                    "{}d {:02}:{:02}:{:02}",
                    seconds / 86_400,
                    seconds / 3_600 % 24,
                    seconds / 60 % 60,
                    seconds % 60
                )
            } else if seconds >= 3_600 {
                format!(
                    "{}:{:02}:{:02}",
                    seconds / 3_600,
                    seconds / 60 % 60,
                    seconds % 60
                )
            } else {
                format!("{:02}:{:02}", seconds / 60, seconds % 60)
            }
        },
    )
}

pub(crate) fn status(status: &AgentStatus) -> (&'static str, Tone) {
    match status {
        AgentStatus::Starting => ("◌ Iniciando", Tone::Warning),
        AgentStatus::Running => ("● Executando", Tone::Primary),
        AgentStatus::Waiting => ("◷ Aguardando", Tone::Muted),
        AgentStatus::Completed => ("✓ Concluído", Tone::Success),
        AgentStatus::Failed => ("! Falhou", Tone::Danger),
        AgentStatus::Cancelled => ("× Cancelado", Tone::Danger),
        AgentStatus::Unknown => ("? Estado não informado", Tone::Muted),
    }
}

pub(crate) fn identity(agent: &AgentActivity) -> String {
    let identity = sanitize(&agent.id);
    if identity.is_empty() {
        "identidade não informada".into()
    } else {
        identity
    }
}

pub(crate) fn title(agent: &AgentActivity) -> String {
    let title = sanitize(&agent.title);
    if title.is_empty() {
        "Tarefa sem título informado".into()
    } else {
        title
    }
}

pub(crate) fn model(agent: &AgentActivity) -> String {
    let model = agent.model.as_deref().map(sanitize).unwrap_or_default();
    let model = if model.is_empty() {
        "Modelo não informado".into()
    } else {
        model
    };
    match agent.effort.as_deref().map(sanitize) {
        Some(effort) if !effort.is_empty() => format!("{model} / {effort}"),
        _ => model,
    }
}

pub(crate) fn preview(agent: &AgentActivity) -> String {
    let preview = sanitize(&agent.preview);
    if preview.is_empty() {
        "Aguardando detalhes da atividade…".into()
    } else {
        preview
    }
}

fn put(frame: &mut Frame, x: usize, y: usize, width: usize, text: &str, tone: Tone) {
    frame.put(x, y, &truncate(&sanitize(text), width), tone);
}

fn truncate(text: &str, width: usize) -> String {
    if cells(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut clipped = String::new();
    let mut used = 0;
    for c in text.chars() {
        let size = c.width().unwrap_or(0);
        if used + size > width - 1 {
            break;
        }
        used += size;
        clipped.push(c);
    }
    clipped.push('…');
    clipped
}

fn cells(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0))
        .sum()
}

/// Word wrapping with cell-width limits, including long commands without spaces.
pub(crate) fn lines(text: &str, width: usize, limit: usize) -> Vec<String> {
    if width == 0 || limit == 0 {
        return Vec::new();
    }
    let text = sanitize(text);
    let mut result = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for word in text.split_whitespace() {
        if !current.is_empty() {
            if used + 1 + cells(word) <= width {
                current.push(' ');
                used += 1;
            } else {
                result.push(std::mem::take(&mut current));
                used = 0;
            }
        }
        for c in word.chars() {
            let size = c.width().unwrap_or(0);
            if used + size > width && !current.is_empty() {
                result.push(std::mem::take(&mut current));
                used = 0;
            }
            if size <= width {
                current.push(c);
                used += size;
            }
        }
        if result.len() > limit {
            break;
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    if result.len() > limit {
        result.truncate(limit);
        let last = result.last_mut().expect("nonzero line limit");
        *last = truncate(&format!("{}…", last.trim_end()), width);
    }
    result
}

/// CLI output is untrusted terminal text: discard escape payloads, control and
/// bidi formatting characters before wrapping. Keep ordinary Unicode content.
pub(crate) fn sanitize(text: &str) -> String {
    enum State {
        Text,
        Escape,
        Sequence,
        String(bool),
    }
    let mut state = State::Text;
    let mut out = String::new();
    let mut pending_space = false;
    for c in text.chars().take(16_384) {
        match state {
            State::Escape => {
                state = match c {
                    '[' => State::Sequence,
                    ']' | 'P' | '^' | '_' => State::String(false),
                    _ => State::Text,
                };
                continue;
            }
            State::Sequence => {
                if ('@'..='~').contains(&c) {
                    state = State::Text;
                }
                continue;
            }
            State::String(escaped) => {
                state = if c == '\u{7}' || c == '\u{9c}' || (escaped && c == '\\') {
                    State::Text
                } else {
                    State::String(c == '\u{1b}')
                };
                continue;
            }
            State::Text => {}
        }
        match c {
            '\u{1b}' => state = State::Escape,
            '\u{9b}' => state = State::Sequence,
            '\u{90}' | '\u{9d}' | '\u{9e}' | '\u{9f}' => state = State::String(false),
            c if c.is_whitespace() => pending_space = !out.is_empty(),
            c if c.is_control()
                || matches!(c, '\u{061c}' | '\u{200b}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}') =>
                {}
            _ => {
                if pending_space {
                    out.push(' ');
                    pending_space = false;
                }
                out.push(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn agent(index: usize) -> AgentActivity {
        AgentActivity {
            id: format!("agent-{index}"),
            parent_id: None,
            title: format!("Investigar tarefa {index}"),
            model: Some("gpt-5.6-luna".into()),
            effort: Some("max".into()),
            preview: "Executando: cargo test --lib".into(),
            status: AgentStatus::Running,
            started_at: None,
            finished_at: None,
        }
    }

    fn contents(frame: &Frame) -> String {
        frame
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn elapsed_time_formats_minutes_hours_days_and_missing_observations() {
        let mut agent = agent(1);
        assert_eq!(elapsed(&agent), "--:--");
        let started = chrono::DateTime::parse_from_rfc3339("2026-09-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        agent.started_at = Some(started);
        agent.status = AgentStatus::Completed;
        for (seconds, expected) in [
            (0, "00:00"),
            (155, "02:35"),
            (3_723, "1:02:03"),
            (93_784, "1d 02:03:04"),
        ] {
            agent.finished_at = Some(started + chrono::Duration::seconds(seconds));
            assert_eq!(elapsed(&agent), expected);
        }
    }

    #[test]
    fn elapsed_time_is_right_aligned_without_overlapping_card_status() {
        let mut agent = agent(1);
        agent.started_at = Some(Utc::now() - chrono::Duration::seconds(155));
        agent.finished_at = Some(agent.started_at.unwrap() + chrono::Duration::seconds(155));
        let mut frame = Frame::blank(120, 24);
        card(&mut frame, 80, 3, 36, &agent);
        let timer = frame
            .spans
            .iter()
            .find(|span| span.text == " 02:35 ")
            .unwrap();
        assert_eq!(timer.y, 3);
        assert_eq!(timer.x + timer.text.width(), 80 + 36 - 2);
        let status = frame
            .spans
            .iter()
            .find(|span| span.text.contains("Executando"))
            .unwrap();
        assert!(status.x + status.text.width() < timer.x);
        for width in 8..55 {
            let mut frame = Frame::blank(80, 24);
            card(&mut frame, 2, 3, width, &agent);
            assert!(
                frame
                    .spans
                    .iter()
                    .all(|span| span.x >= 2 && span.x + span.text.width() <= 2 + width)
            );
        }
    }

    #[test]
    fn sidebar_reserves_chat_space_only_on_wide_terminals() {
        assert_eq!(width(109), None);
        assert_eq!(width(110), Some(36));
        assert_eq!(width(160), Some(40));
        assert_eq!(width(300), Some(54));
        for columns in 110..300 {
            assert!(columns - width(columns).unwrap() - 6 >= 64);
        }
    }

    #[test]
    fn cards_show_observed_title_model_preview_and_status() {
        let mut snapshot = ActivitySnapshot {
            agents: vec![agent(1), agent(2)],
            supported: true,
            omitted: 0,
        };
        snapshot.agents[1].status = AgentStatus::Completed;
        snapshot.agents[1].model = None;
        snapshot.agents[1].effort = None;
        let mut frame = Frame::blank(120, 25);
        draw(&mut frame, 82, 1, 36, 20, &snapshot, &mut 0);
        let rendered = contents(&frame);
        for expected in [
            "SUBAGENTES · 1 ativo",
            "ID · agent-1",
            "Investigar tarefa 1",
            "gpt-5.6-luna / max",
            "Executando: cargo test --lib",
            "✓ Concluído",
            "Modelo não informado",
            "1–2 / 2 · Alt+↑↓",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected}: {rendered}"
            );
        }
        let heading = frame
            .spans
            .iter()
            .find(|span| span.text == "Investigar tarefa 1")
            .unwrap();
        let identity = frame
            .spans
            .iter()
            .find(|span| span.text == "ID · agent-1")
            .unwrap();
        assert!(heading.y < identity.y, "The task must lead the card");
        assert!(
            frame
                .spans
                .iter()
                .all(|span| { span.x >= 82 && span.x + span.text.width() <= 118 && span.y < 21 })
        );
    }

    #[test]
    fn untrusted_cli_text_cannot_inject_terminal_or_bidi_commands() {
        let raw = "\x1b[31mrevisar\x1b[0m\n\x1b]52;c;secret\x07 a\u{202e}pi\x1bPpayload\x1b\\";
        assert_eq!(sanitize(raw), "revisar api");
        assert_eq!(sanitize("\u{9b}31m安全\u{9b}0m\t🦀"), "安全 🦀");
        let mut worker = agent(1);
        worker.title = raw.into();
        worker.model = Some(raw.into());
        worker.preview = raw.into();
        let snapshot = ActivitySnapshot {
            agents: vec![worker],
            supported: true,
            omitted: 0,
        };
        let mut frame = Frame::blank(120, 25);
        draw(&mut frame, 82, 1, 36, 20, &snapshot, &mut 0);
        let rendered = contents(&frame);
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("payload"));
        assert!(!rendered.chars().any(|c| c == '\x1b' || c == '\u{202e}'));
    }

    #[test]
    fn unicode_text_wraps_and_truncates_with_visible_ellipsis() {
        let wrapped = lines("corrigir 安全 API e executar todos os testes", 12, 2);
        assert_eq!(wrapped.len(), 2);
        assert!(wrapped.iter().all(|line| line.width() <= 12));
        assert!(wrapped.last().unwrap().ends_with('…'));
        let long = lines(&"界".repeat(80), 11, 2);
        assert_eq!(long.len(), 2);
        assert!(long.iter().all(|line| line.width() <= 11));
        assert!(long.last().unwrap().ends_with('…'));
        assert_eq!(truncate("安全", 3), "安…");
    }

    #[test]
    fn twenty_agents_scroll_by_card_and_clamp_after_resize() {
        let snapshot = ActivitySnapshot {
            agents: (1..=20).map(agent).collect(),
            supported: true,
            omitted: 0,
        };
        let mut offset = 100;
        let mut frame = Frame::blank(120, 25);
        draw(&mut frame, 82, 1, 36, 20, &snapshot, &mut offset);
        assert_eq!(offset, 18);
        let rendered = contents(&frame);
        assert!(rendered.contains("Investigar tarefa 19"));
        assert!(rendered.contains("Investigar tarefa 20"));
        assert!(rendered.contains("19–20 / 20"));
        let mut larger = Frame::blank(140, 50);
        draw(&mut larger, 90, 1, 48, 44, &snapshot, &mut offset);
        assert_eq!(offset, 15);
        assert!(contents(&larger).contains("16–20 / 20"));
    }

    #[test]
    fn empty_state_never_invents_team_members() {
        let mut snapshot = ActivitySnapshot {
            supported: false,
            ..Default::default()
        };
        let mut offset = 5;
        let mut frame = Frame::blank(120, 25);
        draw(&mut frame, 82, 1, 36, 20, &snapshot, &mut offset);
        assert_eq!(offset, 0);
        assert!(contents(&frame).contains("Este CLI ainda não informou"));
        snapshot.supported = true;
        let mut frame = Frame::blank(120, 25);
        draw(&mut frame, 82, 1, 36, 20, &snapshot, &mut offset);
        assert!(contents(&frame).contains("Nenhum subagente em atividade"));
        assert!(contents(&frame).contains("tarefas delegadas aparecerão"));
        for height in 0..5 {
            let mut frame = Frame::blank(64, 20);
            draw(&mut frame, 2, 4, 36, height, &snapshot, &mut offset);
            assert!(frame.spans.iter().all(|span| span.y < 4 + height));
        }
    }

    #[test]
    fn compact_height_preserves_the_agent_details_without_overflow() {
        let snapshot = ActivitySnapshot {
            agents: vec![agent(1)],
            supported: true,
            omitted: 0,
        };
        for height in 0..24 {
            let mut frame = Frame::blank(120, 30);
            draw(&mut frame, 82, 2, 36, height, &snapshot, &mut 0);
            assert!(frame.spans.iter().all(|span| span.y < height + 2));
            if height >= 6 {
                let rendered = contents(&frame);
                assert!(rendered.contains("Investigar tarefa 1"));
                assert!(rendered.contains("gpt-5.6-luna / max"));
                assert!(rendered.contains("Executando: cargo test --lib"));
            }
        }
    }

    #[test]
    fn terminal_states_remain_distinct_after_the_execution_ends() {
        assert!(status(&AgentStatus::Failed).0.contains("Falhou"));
        assert!(status(&AgentStatus::Cancelled).0.contains("Cancelado"));
        assert!(status(&AgentStatus::Completed).0.contains("Concluído"));
        assert!(status(&AgentStatus::Unknown).0.contains("não informado"));
    }

    #[test]
    fn card_clicks_preserve_agent_ids_after_scroll_and_reordering() {
        let mut snapshot = ActivitySnapshot {
            agents: (1..=20).map(agent).collect(),
            supported: true,
            omitted: 0,
        };
        let mut frame = Frame::blank(120, 25);
        let mut offset = 18;
        let hits = draw(&mut frame, 82, 1, 36, 20, &snapshot, &mut offset);
        assert_eq!(
            hits.iter().map(|(_, id)| id.as_str()).collect::<Vec<_>>(),
            vec!["agent-19", "agent-20"]
        );
        for (rect, _) in &hits {
            assert!(rect.x >= 82 && rect.x + rect.width <= 118);
            assert!(rect.y >= 1 && rect.y + rect.height <= 21);
            assert!(rect.contains(rect.x + rect.width - 1, rect.y + rect.height - 1));
            assert!(!rect.contains(rect.x, rect.y + rect.height));
        }
        snapshot.agents.swap(18, 19);
        let mut reordered = Frame::blank(120, 25);
        let reordered_hits = draw(&mut reordered, 82, 1, 36, 20, &snapshot, &mut offset);
        assert_eq!(reordered_hits[0].1, "agent-20");
        assert_eq!(reordered_hits[1].1, "agent-19");
    }

    #[test]
    fn compact_click_target_excludes_header_and_footer_and_absent_cards() {
        let snapshot = ActivitySnapshot {
            agents: vec![agent(1)],
            supported: true,
            omitted: 0,
        };
        for height in 0..10 {
            let mut frame = Frame::blank(64, 20);
            let hits = draw(&mut frame, 2, 2, 60, height, &snapshot, &mut 0);
            if height >= 6 {
                assert_eq!(hits.len(), 1);
                assert_eq!(hits[0].1, "agent-1");
                assert_eq!(
                    hits[0].0,
                    Rect {
                        x: 2,
                        y: 3,
                        width: 60,
                        height: height - 2
                    }
                );
            } else {
                assert!(hits.is_empty());
            }
        }
        let mut frame = Frame::blank(64, 20);
        assert!(
            draw(
                &mut frame,
                2,
                2,
                60,
                18,
                &ActivitySnapshot::default(),
                &mut 0
            )
            .is_empty()
        );
    }

    #[test]
    fn joined_emoji_titles_follow_the_frame_cell_width() {
        let mut worker = agent(1);
        worker.title = "👩‍💻".repeat(30);
        worker.preview = "👨‍👩‍👧‍👦".repeat(20);
        let snapshot = ActivitySnapshot {
            agents: vec![worker],
            supported: true,
            omitted: 0,
        };
        for width in [12, 36, 60] {
            let mut frame = Frame::blank(120, 25);
            let hits = draw(&mut frame, 2, 2, width, 18, &snapshot, &mut 0);
            assert_eq!(hits.len(), 1);
            assert!(
                frame
                    .spans
                    .iter()
                    .all(|span| span.x + cells(&span.text) <= 2 + width)
            );
        }
    }
}
