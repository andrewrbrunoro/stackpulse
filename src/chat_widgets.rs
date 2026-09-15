//! Compact conversation metrics, with hitboxes in absolute `Frame` coordinates.
use crate::{
    chat_metrics::{ConversationStats, PromptStats},
    chat_tabs::Rect,
    tui::{Frame, Tone},
    widget::{clean, number},
};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WidgetAction {
    Details,
    Previous,
    Next,
    Rate,
}

pub(crate) fn draw(
    frame: &mut Frame,
    area: Rect,
    stats: &ConversationStats,
    selected: usize,
) -> Vec<(Rect, WidgetAction)> {
    let width = area.width.min(frame.width.saturating_sub(area.x));
    let height = area.height.min(frame.height.saturating_sub(area.y));
    if width < 8 || height < 3 {
        return Vec::new();
    }
    let compact = height < 5;
    let height = if compact { 3 } else { 5 };
    let base = (width - 2) / 3;
    let widths = [base, base, width - 2 - 2 * base];
    let mut x = area.x;
    let cards = widths.map(|width| {
        let card = Rect {
            x,
            y: area.y,
            width,
            height,
        };
        x += width + 1;
        card
    });
    let mut hits = vec![(cards[0], WidgetAction::Details)];
    conversation(frame, cards[0], stats, compact);
    let selected = selected.min(stats.prompts.len().saturating_sub(1));
    let prompt = stats.prompts.get(selected);
    tokens(frame, cards[1], stats, prompt, selected, compact, &mut hits);
    rating(frame, cards[2], stats, prompt, compact, &mut hits);
    hits
}

fn conversation(frame: &mut Frame, rect: Rect, stats: &ConversationStats, compact: bool) {
    let body = card(frame, rect, "CONVERSA", compact, Tone::Navigation);
    put(
        frame,
        body,
        0,
        &format!("{} tokens", tokens_value(stats.total_tokens)),
        Tone::Text,
    );
    let count = stats.prompts.len();
    let summary = if compact && stats.partial {
        format!("{count} ped. · parcial")
    } else {
        let full = format!(
            "{count} {} · {} {}",
            if count == 1 { "pedido" } else { "pedidos" },
            stats.measured,
            if stats.measured == 1 {
                "medido"
            } else {
                "medidos"
            }
        );
        if cells(&full) <= body.width {
            full
        } else {
            format!("{count} ped. · {} med.", stats.measured)
        }
    };
    put(frame, body, 1, &summary, Tone::Muted);
    if !compact {
        put(
            frame,
            body,
            2,
            if stats.partial {
                "Parcial · abrir"
            } else {
                "[Detalhes]"
            },
            if stats.partial {
                Tone::Warning
            } else {
                Tone::Navigation
            },
        );
    }
}

fn tokens(
    frame: &mut Frame,
    rect: Rect,
    stats: &ConversationStats,
    prompt: Option<&PromptStats>,
    selected: usize,
    compact: bool,
    hits: &mut Vec<(Rect, WidgetAction)>,
) {
    let body = card(frame, rect, "TOKENS POR PEDIDO", compact, Tone::Navigation);
    let count = stats.prompts.len();
    let value = prompt.map_or_else(
        || "N/D tokens".into(),
        |prompt| {
            format!(
                "#{}/{count} · {}",
                prompt.number,
                tokens_value(prompt.tokens)
            )
        },
    );
    put(frame, body, 0, &value, Tone::Text);
    if !compact {
        let title = prompt.map_or_else(
            || "Nenhum pedido".into(),
            |prompt| {
                if prompt.title.trim().is_empty() {
                    format!("Pedido {}", prompt.number)
                } else {
                    prompt.title.clone()
                }
            },
        );
        put(frame, body, 1, &title, Tone::Muted);
    }
    let y = body.y + body.height.saturating_sub(1);
    button(
        frame,
        body,
        body.x,
        y,
        "[<]",
        (prompt.is_some() && selected > 0).then_some(WidgetAction::Previous),
        hits,
    );
    button(
        frame,
        body,
        body.x + 4,
        y,
        "[>]",
        (prompt.is_some() && selected + 1 < count).then_some(WidgetAction::Next),
        hits,
    );
    if let Some(prompt) = prompt {
        let status = if !prompt.finished {
            "Em curso"
        } else if prompt.partial {
            "Parcial"
        } else {
            "Concluído"
        };
        let rest = Rect {
            x: body.x + 8,
            y,
            width: body.width.saturating_sub(8),
            height: 1,
        };
        put(
            frame,
            rest,
            0,
            status,
            if !prompt.finished || prompt.partial {
                Tone::Warning
            } else {
                Tone::Success
            },
        );
    }
}

fn rating(
    frame: &mut Frame,
    rect: Rect,
    stats: &ConversationStats,
    prompt: Option<&PromptStats>,
    compact: bool,
    hits: &mut Vec<(Rect, WidgetAction)>,
) {
    let body = card(frame, rect, "NOTA DA CONVERSA", compact, Tone::Highlight);
    let valid_score = stats
        .score
        .filter(|score| score.is_finite() && (0.0..=10.0).contains(score));
    let score = valid_score.map_or_else(
        || "N/D".into(),
        |score| format!("{score:.1}").replace('.', ","),
    );
    let count = stats.prompts.len();
    if compact {
        let full = if valid_score.is_some() {
            format!("{score}/10 {}/{count} avaliados", stats.rated)
        } else {
            format!("Sem nota · 0/{count}")
        };
        let short = if valid_score.is_some() {
            format!("{score}/10 {}/{count} aval.", stats.rated)
        } else {
            full.clone()
        };
        put(
            frame,
            body,
            0,
            if cells(&full) <= body.width {
                &full
            } else {
                &short
            },
            Tone::Text,
        );
    } else {
        let value = if valid_score.is_some() {
            format!("{score} / 10")
        } else {
            "Sem nota".into()
        };
        put(frame, body, 0, &value, Tone::Text);
        put(
            frame,
            body,
            1,
            &format!("{}/{count} avaliados", stats.rated),
            Tone::Muted,
        );
    }
    if prompt.is_some() {
        button(
            frame,
            body,
            body.x,
            body.y + body.height.saturating_sub(1),
            "[Avaliar]",
            Some(WidgetAction::Rate),
            hits,
        );
    } else {
        put(
            frame,
            body,
            body.height.saturating_sub(1),
            "Sem avaliação",
            Tone::Muted,
        );
    }
}

fn card(frame: &mut Frame, rect: Rect, title: &str, compact: bool, title_tone: Tone) -> Rect {
    if compact {
        put(frame, rect, 0, title, title_tone);
        return Rect {
            y: rect.y + 1,
            height: 2,
            ..rect
        };
    }
    let inner = rect.width.saturating_sub(2);
    frame.put(
        rect.x,
        rect.y,
        &format!("╭{}╮", "─".repeat(inner)),
        Tone::Muted,
    );
    frame.put(
        rect.x,
        rect.y + 4,
        &format!("╰{}╯", "─".repeat(inner)),
        Tone::Muted,
    );
    for row in 1..4 {
        frame.put(rect.x, rect.y + row, "│", Tone::Muted);
        frame.put(rect.x + rect.width - 1, rect.y + row, "│", Tone::Muted);
    }
    let heading = Rect {
        x: rect.x + 1,
        y: rect.y,
        width: inner,
        height: 1,
    };
    put(frame, heading, 0, title, title_tone);
    Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: inner,
        height: 3,
    }
}

fn button(
    frame: &mut Frame,
    area: Rect,
    x: usize,
    y: usize,
    label: &str,
    action: Option<WidgetAction>,
    hits: &mut Vec<(Rect, WidgetAction)>,
) {
    let width = cells(label);
    if x < area.x
        || y < area.y
        || y - area.y >= area.height
        || width > area.width.saturating_sub(x - area.x)
    {
        return;
    }
    frame.put(
        x,
        y,
        label,
        if action.is_some() {
            Tone::Navigation
        } else {
            Tone::Muted
        },
    );
    if let Some(action) = action {
        hits.push((
            Rect {
                x,
                y,
                width,
                height: 1,
            },
            action,
        ));
    }
}

fn put(frame: &mut Frame, rect: Rect, row: usize, text: &str, tone: Tone) {
    if row >= rect.height || rect.width == 0 {
        return;
    }
    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = clean(&single_line, usize::MAX);
    let text = if cells(&text) > rect.width {
        format!("{}…", clean(&text, rect.width - 1))
    } else {
        text
    };
    frame.put(rect.x, rect.y + row, &text, tone);
}

fn cells(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0))
        .sum()
}

fn tokens_value(tokens: Option<u64>) -> String {
    tokens.map_or_else(|| "N/D".into(), number)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(number: usize, tokens: Option<u64>, can_rate: bool) -> PromptStats {
        PromptStats {
            number,
            title: "Revisão 界面 e\u{301} 👩‍💻 com um título longo\npara o pedido".into(),
            tokens,
            elapsed_ms: None,
            delivery_score: None,
            speed: None,
            finished: true,
            partial: tokens.is_none(),
            execution_id: can_rate.then(|| format!("execution-{number}")),
        }
    }

    fn stats() -> ConversationStats {
        ConversationStats {
            prompts: vec![
                prompt(1, Some(1500), true),
                prompt(2, None, false),
                prompt(3, Some(2500), true),
            ],
            total_tokens: Some(4000),
            measured: 2,
            rated: 2,
            score: Some(8.5),
            partial: true,
        }
    }

    fn texts(frame: &Frame) -> String {
        frame
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn empty_conversation_keeps_tokens_and_rating_unknown() {
        let empty = ConversationStats {
            prompts: vec![],
            total_tokens: None,
            measured: 0,
            rated: 0,
            score: None,
            partial: false,
        };
        for height in [3, 5] {
            let mut frame = Frame::blank(64, 20);
            let hits = draw(
                &mut frame,
                Rect {
                    x: 2,
                    y: 4,
                    width: 60,
                    height,
                },
                &empty,
                99,
            );
            let content = texts(&frame);
            assert!(content.contains("N/D tokens"));
            assert!(content.contains("N/D"));
            assert!(content.contains("0/0"));
            assert!(content.contains("Sem avaliação"));
            assert!(!content.contains("0 tokens"));
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].1, WidgetAction::Details);
            assert!(content.contains("[<]"));
            assert!(content.contains("[>]"));
        }
    }

    #[test]
    fn partial_conversation_preserves_known_sum_and_missing_prompt() {
        for height in [3, 5] {
            let mut frame = Frame::blank(124, 30);
            let hits = draw(
                &mut frame,
                Rect {
                    x: 2,
                    y: 4,
                    width: 120,
                    height,
                },
                &stats(),
                1,
            );
            let content = texts(&frame);
            assert!(content.contains("4.0k tokens"));
            assert!(content.to_lowercase().contains("parcial"));
            assert!(content.contains("#2/3 · N/D"));
            assert!(content.contains("8,5"));
            assert!(content.contains("2/3 avaliados"));
            assert!(
                hits.iter()
                    .any(|(_, action)| *action == WidgetAction::Previous)
            );
            assert!(hits.iter().any(|(_, action)| *action == WidgetAction::Next));
            assert!(hits.iter().any(|(_, action)| *action == WidgetAction::Rate));
        }
    }

    #[test]
    fn selected_prompt_controls_rating_and_navigation() {
        let mut frame = Frame::blank(64, 20);
        let area = Rect {
            x: 2,
            y: 4,
            width: 60,
            height: 5,
        };
        let first = draw(&mut frame, area, &stats(), 0);
        assert!(
            !first
                .iter()
                .any(|(_, action)| *action == WidgetAction::Previous)
        );
        assert!(
            first
                .iter()
                .any(|(_, action)| *action == WidgetAction::Next)
        );
        assert!(
            first
                .iter()
                .any(|(_, action)| *action == WidgetAction::Rate)
        );
        let mut frame = Frame::blank(64, 20);
        let last = draw(&mut frame, area, &stats(), usize::MAX);
        assert!(
            last.iter()
                .any(|(_, action)| *action == WidgetAction::Previous)
        );
        assert!(!last.iter().any(|(_, action)| *action == WidgetAction::Next));
        let (rate, _) = last
            .iter()
            .find(|(_, action)| *action == WidgetAction::Rate)
            .unwrap();
        assert!(
            frame
                .spans
                .iter()
                .any(|span| span.x == rate.x && span.y == rate.y && span.text == "[Avaliar]")
        );
        assert!(rate.contains(rate.x + rate.width - 1, rate.y));
        assert!(!rate.contains(rate.x + rate.width, rate.y));
    }

    #[test]
    fn unicode_cells_and_hitboxes_remain_inside_clipped_geometry() {
        for width in [0, 7, 8, 59, 60, 64, 124, 150] {
            for height in [0, 2, 3, 4, 5, 8] {
                let mut frame = Frame::blank(width, 20);
                let area = Rect {
                    x: 2,
                    y: 4,
                    width: 150,
                    height,
                };
                let hits = draw(&mut frame, area, &stats(), 0);
                for span in &frame.spans {
                    assert!(span.x >= area.x);
                    assert!(span.x + cells(&span.text) <= width);
                    assert!(span.y >= area.y && span.y < area.y + height.min(5));
                    assert!(!span.text.chars().any(char::is_control));
                }
                for (index, (rect, _)) in hits.iter().enumerate() {
                    assert!(rect.x >= area.x && rect.x + rect.width <= width);
                    assert!(rect.y >= area.y && rect.y + rect.height <= area.y + height.min(5));
                    assert!(rect.width > 0 && rect.height > 0);
                    for (other, _) in hits.iter().skip(index + 1) {
                        assert!(rect.x + rect.width <= other.x || other.x + other.width <= rect.x);
                    }
                }
                if width == 64 && height == 5 {
                    assert!(texts(&frame).contains('…'));
                }
            }
        }
    }

    #[test]
    fn valid_zero_scores_and_tokens_remain_distinct_from_missing_values() {
        let mut stats = stats();
        stats.total_tokens = Some(0);
        stats.prompts[0].tokens = Some(0);
        stats.score = Some(0.0);
        let mut frame = Frame::blank(124, 20);
        let area = Rect {
            x: 0,
            y: 4,
            width: 124,
            height: 5,
        };
        draw(&mut frame, area, &stats, 0);
        let content = texts(&frame);
        assert!(content.contains("0 tokens"));
        assert!(content.contains("#1/3 · 0"));
        assert!(content.contains("0,0 / 10"));
        for invalid in [f64::NAN, f64::INFINITY, -1.0, 11.0] {
            stats.score = Some(invalid);
            let mut frame = Frame::blank(124, 20);
            draw(&mut frame, area, &stats, 0);
            assert!(texts(&frame).contains("Sem nota"));
        }
    }

    #[test]
    fn widget_colors_separate_usage_rating_actions_and_status() {
        for height in [3, 5] {
            let mut frame = Frame::blank(124, 20);
            let area = Rect {
                x: 0,
                y: 0,
                width: 124,
                height,
            };
            let stats = stats();
            draw(&mut frame, area, &stats, 0);
            for title in ["CONVERSA", "TOKENS POR PEDIDO"] {
                assert!(
                    frame
                        .spans
                        .iter()
                        .any(|span| span.text == title && span.tone == Tone::Navigation)
                );
            }
            assert!(
                frame
                    .spans
                    .iter()
                    .any(|span| span.text == "NOTA DA CONVERSA" && span.tone == Tone::Highlight)
            );
            assert!(
                frame
                    .spans
                    .iter()
                    .any(|span| span.text == "[Avaliar]" && span.tone == Tone::Navigation)
            );
            assert!(
                frame
                    .spans
                    .iter()
                    .any(|span| span.text == "Concluído" && span.tone == Tone::Success)
            );
            let mut frame = Frame::blank(124, 20);
            draw(&mut frame, area, &stats, 1);
            assert!(
                frame
                    .spans
                    .iter()
                    .any(|span| span.text == "Parcial" && span.tone == Tone::Warning)
            );
        }
    }
}
