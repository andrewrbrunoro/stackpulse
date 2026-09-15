//! Subagent detail surface. Rendering never sends a message or controls an agent.
use crate::{
    activity::AgentActivity,
    chat_agents::{elapsed, identity, lines, model, preview, sanitize, status, title},
    chat_composer::Composer,
    chat_tabs::Rect,
    tui::{Frame, Tone},
    widget::clean,
};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug)]
pub(crate) struct DetailEntry {
    /// A public message or intervention label supplied by the conversation state.
    pub(crate) title: String,
    pub(crate) text: String,
}

pub(crate) struct DetailView<'a> {
    pub(crate) agent: &'a AgentActivity,
    pub(crate) entries: &'a [DetailEntry],
    pub(crate) composer: &'a Composer,
    pub(crate) can_send: bool,
    pub(crate) can_interrupt: bool,
    pub(crate) capability_note: &'a str,
    pub(crate) busy: bool,
    pub(crate) focused: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DetailAction {
    Back,
    Send,
    InterruptRedirect,
    Composer { first: usize, width: usize },
    Content,
}

/// `scroll` counts wrapped public-content lines; usize::MAX follows the bottom.
/// Composer clicks are relative to their hitbox; add `first` to the clicked row
/// before calling `Composer::place_cursor` with the returned wrapping width.
pub(crate) fn draw(
    frame: &mut Frame,
    area: Rect,
    view: DetailView<'_>,
    scroll: &mut usize,
) -> Vec<(Rect, DetailAction)> {
    let area = Rect {
        width: area.width.min(frame.width.saturating_sub(area.x)),
        height: area.height.min(frame.height.saturating_sub(area.y)),
        ..area
    };
    let mut hits = Vec::new();
    if area.width == 0 || area.height == 0 {
        return hits;
    }
    let padding = usize::from(area.width >= 4);
    let inner = Rect {
        x: area.x + padding,
        width: area.width - 2 * padding,
        ..area
    };
    header(frame, inner, view.agent, &mut hits);
    if area.height < 6 || inner.width < 12 {
        if area.height > 3 {
            put(
                frame,
                inner,
                3,
                "Amplie para orientar este agente.",
                Tone::Muted,
            );
        }
        return hits;
    }

    let note = if !view.capability_note.trim().is_empty() {
        sanitize(view.capability_note)
    } else if view.busy {
        "Enviando orientação ao agente…".into()
    } else if !view.can_send && !view.can_interrupt {
        "Envio e redirecionamento indisponíveis.".into()
    } else if view.composer.value.trim().is_empty() {
        "Escreva uma orientação para habilitar as ações disponíveis.".into()
    } else {
        "A orientação será enviada somente a este agente.".into()
    };
    let note_lines = lines(&note, inner.width, if area.height >= 12 { 2 } else { 1 });
    let note_rows = note_lines.len().max(1);
    let send_label = "[Enviar orientação]";
    let redirect_label = "[Interromper e redirecionar]";
    let stacked = inner.width < cells(send_label) + cells(redirect_label) + 1 && area.height >= 8;
    let action_rows = if stacked { 2 } else { 1 };
    let composer_rows = if area.height >= 16 {
        3
    } else if area.height >= 10 {
        2
    } else {
        1
    };
    let note_y = inner.y + inner.height - note_rows;
    let actions_y = note_y - action_rows;
    let composer_y = actions_y - composer_rows;
    let content_y = inner.y + 3;
    let content_height = composer_y.saturating_sub(content_y);
    let content_height = if content_height > 1 {
        put(
            frame,
            Rect {
                y: composer_y - 1,
                height: 1,
                ..inner
            },
            0,
            "ORIENTAR AGENTE · PgUp/PgDn percorrem os detalhes",
            Tone::Accent,
        );
        content_height - 1
    } else {
        content_height
    };
    if content_height > 0 {
        let content = Rect {
            y: content_y,
            height: content_height,
            ..inner
        };
        let rows = public_content(&view, content.width);
        *scroll = (*scroll).min(rows.len().saturating_sub(content.height));
        for (row, (text, tone)) in rows.iter().skip(*scroll).take(content.height).enumerate() {
            put(frame, content, row, text, *tone);
        }
        hits.push((content, DetailAction::Content));
    } else {
        *scroll = 0;
    }

    composer(
        frame,
        Rect {
            y: composer_y,
            height: composer_rows,
            ..inner
        },
        view.composer,
        view.focused,
        &mut hits,
    );
    let ready = !view.busy && !view.composer.value.trim().is_empty();
    let send_action = (view.can_send && ready).then_some(DetailAction::Send);
    let redirect_action = (view.can_interrupt && ready).then_some(DetailAction::InterruptRedirect);
    let action_area = Rect {
        y: actions_y,
        height: 1,
        ..inner
    };
    if stacked {
        button(
            frame,
            action_area,
            send_label,
            Tone::Primary,
            send_action,
            &mut hits,
        );
        button(
            frame,
            Rect {
                y: actions_y + 1,
                ..action_area
            },
            redirect_label,
            Tone::Danger,
            redirect_action,
            &mut hits,
        );
    } else {
        let (send_label, redirect_label) =
            if inner.width > cells(send_label) + cells(redirect_label) {
                (send_label, redirect_label)
            } else {
                ("[Enviar]", "[Parar e orientar]")
            };
        button(
            frame,
            action_area,
            send_label,
            Tone::Primary,
            send_action,
            &mut hits,
        );
        let start = cells(send_label) + 1;
        button(
            frame,
            Rect {
                x: inner.x + start,
                width: inner.width.saturating_sub(start),
                ..action_area
            },
            redirect_label,
            Tone::Danger,
            redirect_action,
            &mut hits,
        );
    }
    let note_area = Rect {
        y: note_y,
        height: note_rows,
        ..inner
    };
    for (row, text) in note_lines.iter().enumerate() {
        put(
            frame,
            note_area,
            row,
            text,
            if !view.can_send && !view.can_interrupt {
                Tone::Warning
            } else {
                Tone::Muted
            },
        );
    }
    hits
}

fn header(
    frame: &mut Frame,
    area: Rect,
    agent: &AgentActivity,
    hits: &mut Vec<(Rect, DetailAction)>,
) {
    let back = if area.width >= cells("[← Voltar à equipe]") {
        "[← Voltar à equipe]"
    } else {
        "[Voltar]"
    };
    button(
        frame,
        area,
        back,
        Tone::Navigation,
        Some(DetailAction::Back),
        hits,
    );
    let start = cells(back) + 2;
    let heading = title(agent);
    put(
        frame,
        Rect {
            x: area.x + start,
            width: area.width.saturating_sub(start),
            height: 1,
            ..area
        },
        0,
        &heading,
        Tone::Accent,
    );
    if area.height > 1 {
        let (label, tone) = status(&agent.status);
        let label = format!("{label} · {}", elapsed(agent));
        let status_width = cells(&label).min(area.width);
        let model_width = area.width.saturating_sub(status_width + 2);
        let model = model(agent);
        let labeled_model = format!("Modelo: {model}");
        put(
            frame,
            Rect {
                width: model_width,
                ..area
            },
            1,
            if cells(&labeled_model) <= model_width {
                &labeled_model
            } else {
                &model
            },
            Tone::Muted,
        );
        put(
            frame,
            Rect {
                x: area.x + area.width - status_width,
                width: status_width,
                ..area
            },
            1,
            &label,
            tone,
        );
    }
    if area.height > 2 {
        put(
            frame,
            area,
            2,
            &format!("ATIVIDADE ATUAL · {}", preview(agent)),
            if agent.preview.trim().is_empty() {
                Tone::Muted
            } else {
                Tone::Text
            },
        );
    }
}

fn public_content(view: &DetailView<'_>, width: usize) -> Vec<(String, Tone)> {
    let mut result = Vec::new();
    result.push(("TAREFA RECEBIDA".into(), Tone::Accent));
    result.extend(
        lines(&title(view.agent), width, 1024)
            .into_iter()
            .map(|line| (line, Tone::Text)),
    );
    result.extend(
        lines(&format!("ID · {}", identity(view.agent)), width, 128)
            .into_iter()
            .map(|line| (line, Tone::Muted)),
    );
    result.push((String::new(), Tone::Muted));
    result.push(("ATIVIDADE MAIS RECENTE".into(), Tone::Accent));
    result.extend(
        lines(&preview(view.agent), width, 1024)
            .into_iter()
            .map(|line| (line, Tone::Text)),
    );
    result.push((String::new(), Tone::Muted));
    result.push(("HISTÓRICO DE ORIENTAÇÕES".into(), Tone::Accent));
    if view.entries.is_empty() {
        result.push((
            "Nenhuma orientação enviada nesta tarefa.".into(),
            Tone::Muted,
        ));
    }
    for entry in view.entries {
        result.push((String::new(), Tone::Muted));
        result.extend(
            lines(&entry.title, width, 128)
                .into_iter()
                .map(|line| (line, Tone::Navigation)),
        );
        result.extend(
            lines(&entry.text, width, 1024)
                .into_iter()
                .map(|line| (line, Tone::Text)),
        );
    }
    result
}

fn composer(
    frame: &mut Frame,
    area: Rect,
    composer: &Composer,
    focused: bool,
    hits: &mut Vec<(Rect, DetailAction)>,
) {
    let input = Rect {
        x: area.x + 2,
        width: area.width.saturating_sub(2),
        ..area
    };
    let (rows, caret) = composer.lines(input.width);
    let first = caret
        .1
        .saturating_sub(input.height - 1)
        .min(rows.len().saturating_sub(input.height));
    put(
        frame,
        area,
        0,
        "›",
        if focused {
            Tone::Navigation
        } else {
            Tone::Muted
        },
    );
    if composer.value.is_empty() {
        put(
            frame,
            input,
            0,
            "Escreva uma orientação para este agente…",
            Tone::Muted,
        );
    } else {
        // Composer already normalizes controls and maps the caret by cells.
        for (row, text) in rows.iter().skip(first).take(input.height).enumerate() {
            frame.put(
                input.x,
                input.y + row,
                &clean(text, input.width),
                Tone::Text,
            );
        }
    }
    if focused {
        frame.caret = Some((
            input.x + caret.0.min(input.width - 1),
            input.y + caret.1 - first,
        ));
    }
    hits.push((
        input,
        DetailAction::Composer {
            first,
            width: input.width,
        },
    ));
}

fn button(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    tone: Tone,
    action: Option<DetailAction>,
    hits: &mut Vec<(Rect, DetailAction)>,
) {
    let width = cells(label);
    if area.height == 0 || width > area.width {
        return;
    }
    frame.put(
        area.x,
        area.y,
        label,
        if action.is_some() { tone } else { Tone::Muted },
    );
    if let Some(action) = action {
        hits.push((
            Rect {
                width,
                height: 1,
                ..area
            },
            action,
        ));
    }
}

fn put(frame: &mut Frame, area: Rect, row: usize, text: &str, tone: Tone) {
    if area.width == 0 || row >= area.height {
        return;
    }
    let text = sanitize(text);
    let clipped = if cells(&text) <= area.width {
        text
    } else {
        format!("{}…", clean(&text, area.width - 1))
    };
    frame.put(area.x, area.y + row, &clipped, tone);
}

fn cells(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::AgentStatus;

    fn agent() -> AgentActivity {
        AgentActivity {
            id: "agent-stable-19".into(),
            parent_id: Some("root".into()),
            title: "Verificar testes e arquivos 界面".into(),
            model: Some("gpt-5.6-luna".into()),
            effort: Some("max".into()),
            preview: "Executando cargo test --lib".into(),
            status: AgentStatus::Running,
            started_at: None,
            finished_at: None,
        }
    }

    fn view<'a>(
        agent: &'a AgentActivity,
        entries: &'a [DetailEntry],
        composer: &'a Composer,
    ) -> DetailView<'a> {
        DetailView {
            agent,
            entries,
            composer,
            can_send: true,
            can_interrupt: true,
            capability_note: "",
            busy: false,
            focused: true,
        }
    }

    fn text(frame: &Frame) -> String {
        frame
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn detail_shows_observed_activity_and_real_interventions() {
        let agent = agent();
        let entries = vec![DetailEntry {
            title: "Orientação enviada".into(),
            text: "Priorize somente os testes de autenticação.".into(),
        }];
        let mut composer = Composer::default();
        composer
            .insert("Depois verifique os erros de login.")
            .unwrap();
        let mut frame = Frame::blank(100, 30);
        let hits = draw(
            &mut frame,
            Rect {
                x: 2,
                y: 1,
                width: 96,
                height: 28,
            },
            view(&agent, &entries, &composer),
            &mut 0,
        );
        let content = text(&frame);
        for expected in [
            "Voltar à equipe",
            "ID · agent-stable-19",
            "Verificar testes",
            "gpt-5.6-luna / max",
            "Executando",
            "cargo test --lib",
            "ATIVIDADE MAIS RECENTE",
            "HISTÓRICO DE ORIENTAÇÕES",
            "Orientação enviada",
            "Priorize somente",
            "Enviar orientação",
            "Interromper e redirecionar",
        ] {
            assert!(content.contains(expected), "missing {expected}: {content}");
        }
        assert!(hits.iter().any(|(_, action)| *action == DetailAction::Send));
        assert!(
            hits.iter()
                .any(|(_, action)| *action == DetailAction::InterruptRedirect)
        );
        assert!(hits.iter().any(|(_, action)| *action == DetailAction::Back));
    }

    #[test]
    fn unsupported_actions_have_no_hits_but_draft_and_capability_remain_visible() {
        let agent = agent();
        let mut composer = Composer::default();
        composer.insert("Rascunho preservado").unwrap();
        let mut view = view(&agent, &[], &composer);
        view.can_send = false;
        view.can_interrupt = false;
        view.capability_note = "Este CLI não permite orientar subagentes.";
        let mut frame = Frame::blank(64, 20);
        let hits = draw(
            &mut frame,
            Rect {
                x: 2,
                y: 5,
                width: 60,
                height: 6,
            },
            view,
            &mut 0,
        );
        let content = text(&frame);
        assert!(content.contains("Rascunho preservado"));
        assert!(content.contains("Este CLI não permite"));
        assert!(content.contains("Enviar orientação"));
        assert!(content.contains("Interromper e redirecionar"));
        assert!(!hits.iter().any(|(_, action)| matches!(
            action,
            DetailAction::Send | DetailAction::InterruptRedirect
        )));
        assert!(
            hits.iter()
                .any(|(_, action)| matches!(action, DetailAction::Composer { .. }))
        );
        assert!(frame.caret.is_some());
        assert!(
            frame
                .spans
                .iter()
                .filter(|span| span.text.starts_with("[Enviar")
                    || span.text.starts_with("[Interromper"))
                .all(|span| span.tone == Tone::Muted)
        );
    }

    #[test]
    fn controls_and_caret_stay_inside_small_resized_and_offset_areas() {
        let agent = agent();
        let mut composer = Composer::default();
        composer
            .insert(&"Orientação 界 e\u{301}\n".repeat(12))
            .unwrap();
        for width in [0, 8, 12, 28, 36, 60, 96] {
            for height in 0..22 {
                let mut frame = Frame::blank(100, 24);
                let area = Rect {
                    x: 2,
                    y: 2,
                    width,
                    height,
                };
                let mut scroll = usize::MAX;
                let hits = draw(&mut frame, area, view(&agent, &[], &composer), &mut scroll);
                for span in &frame.spans {
                    assert!(span.x >= area.x && span.x + cells(&span.text) <= area.x + area.width);
                    assert!(span.y >= area.y && span.y < area.y + area.height);
                }
                for (rect, _) in &hits {
                    assert!(rect.x >= area.x && rect.x + rect.width <= area.x + area.width);
                    assert!(rect.y >= area.y && rect.y + rect.height <= area.y + area.height);
                    assert!(rect.width > 0 && rect.height > 0);
                }
                for (index, (rect, _)) in hits.iter().enumerate() {
                    for (other, _) in hits.iter().skip(index + 1) {
                        assert!(
                            rect.x + rect.width <= other.x
                                || other.x + other.width <= rect.x
                                || rect.y + rect.height <= other.y
                                || other.y + other.height <= rect.y
                        );
                    }
                }
                if let Some((x, y)) = frame.caret {
                    assert!(hits.iter().any(|(rect, action)| matches!(
                        action,
                        DetailAction::Composer { .. }
                    ) && rect.contains(x, y)));
                }
            }
        }
    }

    #[test]
    fn public_history_scrolls_without_moving_the_live_preview_or_composer() {
        let mut agent = agent();
        let entries = (0..30)
            .map(|index| DetailEntry {
                title: format!("Orientação {index}"),
                text: format!("Mensagem pública {index}"),
            })
            .collect::<Vec<_>>();
        let composer = Composer::default();
        let mut frame = Frame::blank(64, 20);
        let area = Rect {
            x: 2,
            y: 1,
            width: 60,
            height: 18,
        };
        let mut scroll = usize::MAX;
        draw(
            &mut frame,
            area,
            view(&agent, &entries, &composer),
            &mut scroll,
        );
        assert!(scroll > 0 && scroll < usize::MAX);
        assert!(text(&frame).contains("Mensagem pública 29"));
        agent.preview = "Revisando resultado dos testes".into();
        let mut updated = Frame::blank(64, 20);
        draw(
            &mut updated,
            area,
            view(&agent, &entries, &composer),
            &mut scroll,
        );
        assert!(text(&updated).contains("ATIVIDADE ATUAL · Revisando resultado dos testes"));
        assert!(text(&updated).contains("Mensagem pública 29"));
    }

    #[test]
    fn blank_or_pending_drafts_disable_send_and_redirect_without_erasing_them() {
        let agent = agent();
        let mut composer = Composer::default();
        for busy in [false, true] {
            if busy {
                composer
                    .insert("Texto que ainda está sendo enviado")
                    .unwrap();
            }
            let mut view = view(&agent, &[], &composer);
            view.busy = busy;
            let mut frame = Frame::blank(64, 20);
            let hits = draw(
                &mut frame,
                Rect {
                    x: 0,
                    y: 0,
                    width: 64,
                    height: 20,
                },
                view,
                &mut 0,
            );
            assert!(!hits.iter().any(|(_, action)| matches!(
                action,
                DetailAction::Send | DetailAction::InterruptRedirect
            )));
            assert!(
                hits.iter()
                    .any(|(_, action)| matches!(action, DetailAction::Composer { .. }))
            );
        }
        assert_eq!(composer.value, "Texto que ainda está sendo enviado");
    }
}
