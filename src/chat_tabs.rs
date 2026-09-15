//! Two-line conversation tabs. Hitboxes use the same cell coordinates as `Frame`.
use crate::{
    tui::{Frame, Tone},
    widget::clean,
};
use unicode_width::UnicodeWidthChar;

const TAB_WIDTH: usize = 21;
const NEW_LABEL: &str = "[+ Nova]";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TabLabel {
    pub(crate) title: String,
    pub(crate) running: bool,
    pub(crate) unread: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TabAction {
    Select(usize),
    New,
    Close,
    Previous,
    Next,
    Settings,
    Rename,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Rect {
    pub(crate) x: usize,
    pub(crate) y: usize,
    pub(crate) width: usize,
    pub(crate) height: usize,
}

impl Rect {
    pub(crate) fn contains(self, x: usize, y: usize) -> bool {
        x >= self.x && x - self.x < self.width && y >= self.y && y - self.y < self.height
    }
}

/// Select actions retain the original slice index, including when tabs overflow.
/// Previous/Next select the adjacent conversation; the visible window follows it.
#[allow(dead_code)] // Compatibility entry point for surfaces without Settings.
pub(crate) fn draw(
    frame: &mut Frame,
    y: usize,
    tabs: &[TabLabel],
    active: usize,
) -> Vec<(Rect, TabAction)> {
    draw_inner(frame, y, tabs, active, None)
}

/// Settings remains visible after the conversation strip even when it overflows.
/// `active` still addresses conversations; selecting Settings never shifts indices.
pub(crate) fn draw_with_settings(
    frame: &mut Frame,
    y: usize,
    tabs: &[TabLabel],
    active: usize,
    settings_active: bool,
) -> Vec<(Rect, TabAction)> {
    draw_inner(frame, y, tabs, active, Some(settings_active))
}

fn draw_inner(
    frame: &mut Frame,
    y: usize,
    tabs: &[TabLabel],
    active: usize,
    settings: Option<bool>,
) -> Vec<(Rect, TabAction)> {
    let mut hits = Vec::new();
    if y >= frame.height || frame.width == 0 {
        return hits;
    }
    let active = active.min(tabs.len().saturating_sub(1));
    let new_width = cells(NEW_LABEL);
    let settings_active = settings == Some(true);
    // Below the supported terminal size, keep a visible active tab if possible.
    if frame.width < new_width + 10 && !tabs.is_empty() {
        if settings_active {
            settings_tab(frame, &mut hits, 0, y, frame.width, true);
        } else {
            tab(
                frame,
                &mut hits,
                0,
                y,
                frame.width,
                &tabs[active],
                active,
                true,
            );
        }
        return hits;
    }
    if frame.width < new_width {
        return hits;
    }
    let new_x = frame.width - new_width;
    let close_x = (tabs.len() > 1).then(|| new_x - 4);
    let rename_label = if frame.width < 90 { "[T]" } else { "[Título]" };
    let rename_space = cells(rename_label) + 1;
    let rename_x = settings
        .filter(|_| !tabs.is_empty())
        .map(|_| close_x.unwrap_or(new_x))
        .and_then(|x| x.checked_sub(rename_space));
    let actions_x = rename_x.or(close_x).unwrap_or(new_x);
    let available = actions_x.saturating_sub(1);
    // Reserve Settings independently from the scrolling conversations. At tiny
    // widths it shrinks before sacrificing the currently selected conversation.
    let settings_width = settings.map(|_| TAB_WIDTH.min(available.saturating_sub(10)));
    let settings_x = settings_width.map(|width| available.saturating_sub(width));
    let available = settings_x.map_or(available, |x| x.saturating_sub(1));
    let natural_width = tabs.len().saturating_mul(TAB_WIDTH + 1).saturating_sub(1);
    let overflow = natural_width > available;
    let (start_x, end_x) = if overflow && available >= 9 {
        button(
            frame,
            &mut hits,
            0,
            y,
            "[<]",
            (active > 0).then_some(TabAction::Previous),
        );
        button(
            frame,
            &mut hits,
            available - 3,
            y,
            "[>]",
            (active + 1 < tabs.len()).then_some(TabAction::Next),
        );
        (4, available - 4)
    } else {
        (0, available)
    };
    let area_width = end_x.saturating_sub(start_x);
    let mut conversation_end = start_x;
    if !tabs.is_empty() && area_width > 0 {
        let shown = ((area_width + 1) / (TAB_WIDTH + 1)).max(1).min(tabs.len());
        let first = active.saturating_sub(shown / 2).min(tabs.len() - shown);
        let tab_width = TAB_WIDTH.min((area_width + 1) / shown - 1);
        for (slot, label) in tabs.iter().skip(first).take(shown).enumerate() {
            let index = first + slot;
            tab(
                frame,
                &mut hits,
                start_x + slot * (tab_width + 1),
                y,
                tab_width,
                label,
                index,
                index == active && !settings_active,
            );
            conversation_end = start_x + slot * (tab_width + 1) + tab_width;
        }
    }
    if let (Some(width), Some(reserved_x)) = (settings_width, settings_x) {
        let x = if overflow {
            reserved_x
        } else if tabs.is_empty() {
            0
        } else {
            (conversation_end + 1).min(reserved_x)
        };
        settings_tab(frame, &mut hits, x, y, width, settings_active);
    }
    if let Some(close_x) = close_x {
        button(
            frame,
            &mut hits,
            close_x,
            y,
            "[×]",
            (!settings_active).then_some(TabAction::Close),
        );
    }
    if let Some(x) = rename_x {
        button(
            frame,
            &mut hits,
            x,
            y,
            rename_label,
            (!settings_active).then_some(TabAction::Rename),
        );
    }
    button(frame, &mut hits, new_x, y, NEW_LABEL, Some(TabAction::New));
    hits
}

#[allow(clippy::too_many_arguments)]
fn tab(
    frame: &mut Frame,
    hits: &mut Vec<(Rect, TabAction)>,
    x: usize,
    y: usize,
    width: usize,
    label: &TabLabel,
    index: usize,
    active: bool,
) {
    if width == 0 {
        return;
    }
    let normalized = label.title.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = clean(&normalized, usize::MAX);
    let title = if normalized.trim().is_empty() {
        format!("Conversa {}", index + 1)
    } else {
        normalized
    };
    let marker = match (label.running, label.unread) {
        (true, true) => "●• ",
        (true, false) => "● ",
        (false, true) => "• ",
        (false, false) => "",
    };
    let title = format!("{marker}{title}");
    let text = tab_text(&title, width, active);
    let tone = if active {
        Tone::Selected
    } else if label.running {
        Tone::Warning
    } else if label.unread {
        Tone::Highlight
    } else {
        Tone::Navigation
    };
    frame.put(x, y, &text, tone);
    underline(frame, x, y, cells(&text), active);
    hits.push((
        Rect {
            x,
            y,
            width: cells(&text),
            height: 2.min(frame.height.saturating_sub(y)),
        },
        TabAction::Select(index),
    ));
}

fn tab_text(title: &str, width: usize, active: bool) -> String {
    let arrow = if active { "▶ " } else { "" };
    let prefix = format!("[ {arrow}");
    let overhead = cells(&prefix) + 2;
    if width > overhead {
        let title = ellipsis(title, width - overhead);
        let padding = width - overhead - cells(&title);
        format!("{prefix}{title}{} ]", " ".repeat(padding))
    } else {
        ellipsis(&format!("{arrow}{title}"), width)
    }
}

fn underline(frame: &mut Frame, x: usize, y: usize, width: usize, active: bool) {
    if y + 1 < frame.height {
        frame.put(
            x,
            y + 1,
            &if active { "━" } else { "─" }.repeat(width),
            if active {
                Tone::Navigation
            } else {
                Tone::Muted
            },
        );
    }
}

fn settings_tab(
    frame: &mut Frame,
    hits: &mut Vec<(Rect, TabAction)>,
    x: usize,
    y: usize,
    width: usize,
    active: bool,
) {
    if width == 0 {
        return;
    }
    let text = tab_text("Configurações", width, active);
    frame.put(
        x,
        y,
        &text,
        if active {
            Tone::Selected
        } else {
            Tone::Highlight
        },
    );
    underline(frame, x, y, cells(&text), active);
    hits.push((
        Rect {
            x,
            y,
            width: cells(&text),
            height: 2.min(frame.height.saturating_sub(y)),
        },
        TabAction::Settings,
    ));
}

fn button(
    frame: &mut Frame,
    hits: &mut Vec<(Rect, TabAction)>,
    x: usize,
    y: usize,
    label: &str,
    action: Option<TabAction>,
) {
    let width = cells(label);
    if width > frame.width.saturating_sub(x) || y >= frame.height {
        return;
    }
    frame.put(
        x,
        y,
        label,
        match action {
            Some(TabAction::New) => Tone::Primary,
            Some(TabAction::Close) => Tone::Danger,
            Some(_) => Tone::Navigation,
            None => Tone::Muted,
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

fn cells(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

fn ellipsis(text: &str, width: usize) -> String {
    if cells(text) <= width {
        text.into()
    } else if width == 0 {
        String::new()
    } else {
        format!("{}…", clean(text, width - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(count: usize) -> Vec<TabLabel> {
        (0..count)
            .map(|index| TabLabel {
                title: format!("Conversa {}", index + 1),
                running: index == 1,
                unread: index == 2,
            })
            .collect()
    }

    fn selected(hits: &[(Rect, TabAction)]) -> Vec<usize> {
        hits.iter()
            .filter_map(|(_, action)| match action {
                TabAction::Select(index) => Some(*index),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn five_tabs_fit_a_wide_terminal_without_navigation() {
        let mut frame = Frame::blank(124, 20);
        let hits = draw(&mut frame, 2, &labels(5), 2);
        assert_eq!(selected(&hits), vec![0, 1, 2, 3, 4]);
        assert!(hits.iter().any(|(_, action)| *action == TabAction::New));
        let close = hits
            .iter()
            .find(|(_, action)| *action == TabAction::Close)
            .unwrap()
            .0;
        let new = hits
            .iter()
            .find(|(_, action)| *action == TabAction::New)
            .unwrap()
            .0;
        assert_eq!(close.x + close.width + 1, new.x);
        assert_eq!(new.x + new.width, frame.width);
        assert!(
            !hits
                .iter()
                .any(|(_, action)| matches!(action, TabAction::Previous | TabAction::Next))
        );
        assert_eq!(
            frame
                .spans
                .iter()
                .filter(|span| span.tone == Tone::Selected)
                .count(),
            1
        );
    }

    #[test]
    fn overflow_keeps_active_visible_and_original_indices_clickable() {
        let tabs = labels(20);
        for width in [64, 124] {
            for active in 0..tabs.len() {
                let mut frame = Frame::blank(width, 20);
                let hits = draw(&mut frame, 3, &tabs, active);
                let visible = selected(&hits);
                assert_eq!(visible.len(), if width == 64 { 2 } else { 4 });
                assert!(visible.contains(&active));
                assert!(
                    visible
                        .windows(2)
                        .all(|indices| indices[1] == indices[0] + 1)
                );
                assert_eq!(
                    hits.iter()
                        .any(|(_, action)| *action == TabAction::Previous),
                    active > 0
                );
                assert_eq!(
                    hits.iter().any(|(_, action)| *action == TabAction::Next),
                    active + 1 < tabs.len()
                );
                for (rect, _) in &hits {
                    assert!(rect.contains(rect.x, rect.y));
                    assert!(rect.contains(rect.x + rect.width - 1, rect.y));
                    assert!(!rect.contains(rect.x + rect.width, rect.y));
                    assert!(rect.contains(rect.x, rect.y + rect.height - 1));
                    assert!(!rect.contains(rect.x, rect.y + rect.height));
                }
            }
        }
    }

    #[test]
    fn unicode_titles_are_ellipsized_by_cells_and_show_activity() {
        let tabs = vec![
            TabLabel {
                title: "界面 revisión e\u{301} 👩‍💻 muito longa\nmais texto".into(),
                running: true,
                unread: true,
            },
            TabLabel {
                title: "".into(),
                running: false,
                unread: true,
            },
        ];
        let mut frame = Frame::blank(64, 20);
        let hits = draw(&mut frame, 1, &tabs, 1);
        let tab_spans: Vec<_> = frame
            .spans
            .iter()
            .filter(|span| span.text.starts_with("[ "))
            .collect();
        assert_eq!(tab_spans.len(), 2);
        assert!(tab_spans[0].text.contains("●• "));
        assert!(tab_spans[0].text.contains('…'));
        assert_eq!(tab_spans[0].tone, Tone::Warning);
        assert!(tab_spans[1].text.contains("• Conversa 2"));
        assert_eq!(tab_spans[1].tone, Tone::Selected);
        assert!(tab_spans[1].text.contains("▶ "));
        for span in tab_spans {
            let rect = hits.iter().find(|(rect, _)| rect.x == span.x).unwrap().0;
            assert_eq!(cells(&span.text), rect.width);
            assert_eq!(rect.width, TAB_WIDTH);
            assert!(!span.text.chars().any(char::is_control));
        }
    }

    #[test]
    fn empty_tiny_and_invalid_active_inputs_never_create_phantom_hitboxes() {
        for width in 0..130 {
            for count in [0, 1, 3, 20] {
                for y in [0, 2] {
                    let mut frame = Frame::blank(width, 2);
                    let hits = draw(&mut frame, y, &labels(count), usize::MAX);
                    for (index, (rect, _)) in hits.iter().enumerate() {
                        assert!(rect.width > 0);
                        assert!(rect.x + rect.width <= width);
                        assert!(rect.y < frame.height);
                        assert!(frame.spans.iter().any(|span| span.x == rect.x
                            && span.y == rect.y
                            && cells(&span.text) == rect.width));
                        for (other, _) in hits.iter().skip(index + 1) {
                            assert!(
                                rect.x + rect.width <= other.x || other.x + other.width <= rect.x
                            );
                        }
                    }
                    if count > 0 && width >= 64 && y == 0 {
                        assert!(selected(&hits).contains(&(count - 1)));
                    }
                }
            }
        }
    }

    #[test]
    fn settings_stays_visible_after_overflow_and_never_reindexes_conversations() {
        for width in [64, 86, 124] {
            for active in [0, 7, 19] {
                for settings_active in [false, true] {
                    let mut frame = Frame::blank(width, 20);
                    let hits =
                        draw_with_settings(&mut frame, 6, &labels(20), active, settings_active);
                    assert!(selected(&hits).contains(&active));
                    let settings = hits
                        .iter()
                        .find(|(_, action)| *action == TabAction::Settings)
                        .unwrap()
                        .0;
                    assert_eq!(settings.height, 2);
                    assert!(settings.contains(settings.x, 7));
                    assert_eq!(
                        hits.iter()
                            .filter(|(_, action)| *action == TabAction::Settings)
                            .count(),
                        1
                    );
                    assert_eq!(
                        hits.iter().any(|(_, action)| *action == TabAction::Close),
                        !settings_active
                    );
                    for (rect, action) in &hits {
                        if matches!(action, TabAction::Select(_)) {
                            assert!(rect.x + rect.width <= settings.x);
                        }
                    }
                    let selected: Vec<_> = frame
                        .spans
                        .iter()
                        .filter(|span| span.tone == Tone::Selected)
                        .collect();
                    assert_eq!(selected.len(), 1);
                    assert!(selected[0].text.contains('▶'));
                    if settings_active {
                        assert!(selected[0].text.contains("Configurações"));
                    }
                }
            }
        }
    }

    #[test]
    fn settings_geometry_is_bounded_including_single_row_and_tiny_terminals() {
        for width in 0..130 {
            for count in [0, 1, 20] {
                for y in [0, 1, 2] {
                    let mut frame = Frame::blank(width, 2);
                    let hits = draw_with_settings(&mut frame, y, &labels(count), usize::MAX, true);
                    for (index, (rect, _)) in hits.iter().enumerate() {
                        assert!(rect.width > 0 && rect.height > 0);
                        assert!(rect.x + rect.width <= width);
                        assert!(rect.y + rect.height <= frame.height);
                        for (other, _) in hits.iter().skip(index + 1) {
                            assert!(
                                rect.x + rect.width <= other.x || other.x + other.width <= rect.x
                            );
                        }
                    }
                    if width >= 64 && y < 2 {
                        assert!(
                            hits.iter()
                                .any(|(_, action)| *action == TabAction::Settings)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn tabs_have_visible_underlines_and_semantic_action_tones() {
        let mut frame = Frame::blank(124, 20);
        let hits = draw_with_settings(&mut frame, 6, &labels(2), 0, false);
        for (rect, action) in &hits {
            if matches!(action, TabAction::Select(_) | TabAction::Settings) {
                assert_eq!(rect.height, 2);
                assert!(frame.spans.iter().any(|span| span.x == rect.x
                    && span.y == 7
                    && cells(&span.text) == rect.width));
            }
        }
        assert!(
            frame
                .spans
                .iter()
                .any(|span| span.y == 7 && span.text.contains('━'))
        );
        assert!(
            frame
                .spans
                .iter()
                .any(|span| span.text == NEW_LABEL && span.tone == Tone::Primary)
        );
        assert!(
            frame
                .spans
                .iter()
                .any(|span| span.text == "[×]" && span.tone == Tone::Danger)
        );
    }
}
