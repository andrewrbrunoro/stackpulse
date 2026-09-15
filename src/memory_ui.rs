//! Full-screen selection of optional packages, with attribution before confirmation.
use crate::{
    memory::{CREATOR_URL, PROJECT_URL},
    tui::{self, Frame, TerminalGuard, Tone},
};
use anyhow::{Result, ensure};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const MIN_COLUMNS: u16 = 56;
const MIN_ROWS: u16 = 20;

/// Select a package without installing it or configuring any agent.
/// The caller owns the plain-terminal fallback and subsequent installation.
pub fn select(default: bool) -> Result<bool> {
    select_package(default, Package::Memory)
}

pub fn select_usagebar(default: bool) -> Result<bool> {
    if !crate::usagebar::supported() {
        return Ok(false);
    }
    select_package(default, Package::UsageBar)
}

/// Select all compatible packages on one page. Explicit flags remain fixed.
/// `None` means the page was skipped; the caller preserves explicit flag choices.
pub fn select_plugins(
    memory: Option<bool>,
    usagebar: Option<bool>,
) -> Result<Option<(bool, bool)>> {
    let mut selection = PluginSelection::new(memory, usagebar, crate::usagebar::supported())?;
    let interrupted = Arc::new(AtomicBool::new(false));
    let _signals = Signals::register(interrupted.clone())?;
    let _terminal = TerminalGuard::enter()?;
    let mut redraw = true;
    loop {
        ensure!(
            !interrupted.load(Ordering::Relaxed),
            "Seleção interrompida."
        );
        let (columns, rows) = terminal::size()?;
        if redraw {
            selection
                .frame(columns, rows)
                .draw(columns, rows, tui::colors())?;
            redraw = false;
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        ensure!(
            !interrupted.load(Ordering::Relaxed),
            "Seleção interrompida."
        );
        let event = event::read()?;
        redraw = true;
        if let Event::Key(key) = event {
            let (columns, rows) = terminal::size()?;
            match selection.key(key, columns, rows)? {
                Some(PluginAction::Continue(memory, usagebar)) => {
                    return Ok(Some((memory, usagebar)));
                }
                Some(PluginAction::Skip) => return Ok(None),
                None => {}
            }
        }
    }
}

fn select_package(default: bool, package: Package) -> Result<bool> {
    let interrupted = Arc::new(AtomicBool::new(false));
    let _signals = Signals::register(interrupted.clone())?;
    let _terminal = TerminalGuard::enter()?;
    let mut selection = Selection {
        selected: default,
        package,
    };
    let mut redraw = true;
    loop {
        ensure!(
            !interrupted.load(Ordering::Relaxed),
            "Seleção interrompida."
        );
        let (columns, rows) = terminal::size()?;
        if redraw {
            selection
                .frame(columns, rows)
                .draw(columns, rows, tui::colors())?;
            redraw = false;
        }
        if event::poll(Duration::from_millis(100))? {
            ensure!(
                !interrupted.load(Ordering::Relaxed),
                "Seleção interrompida."
            );
            let event = event::read()?;
            redraw = true;
            match event {
                Event::Key(key) => {
                    // A resize can occur after drawing and before the key arrives.
                    let (columns, rows) = terminal::size()?;
                    if let Some(selected) = selection.key(key, columns, rows)? {
                        return Ok(selected);
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Package {
    Memory,
    UsageBar,
}

impl Package {
    fn name(self) -> &'static str {
        match self {
            Self::Memory => "AI-Memory",
            Self::UsageBar => "AI-UsageBar",
        }
    }

    fn url(self) -> &'static str {
        match self {
            Self::Memory => PROJECT_URL,
            Self::UsageBar => crate::usagebar::PROJECT_URL,
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Memory => "Memória de longo prazo para agentes e times.",
            Self::UsageBar => "Consumo dos CLIs em texto ou painel TUI.",
        }
    }

    fn scope(self) -> &'static str {
        match self {
            Self::Memory => "Conexão aos agentes é uma etapa separada.",
            Self::UsageBar => "Barra gráfica: configure separadamente.",
        }
    }
}

#[derive(Debug, PartialEq)]
enum PluginAction {
    Continue(bool, bool),
    Skip,
}

struct PluginSelection {
    memory: bool,
    usagebar: bool,
    memory_fixed: bool,
    usagebar_fixed: bool,
    usagebar_supported: bool,
    focused: usize,
}

impl PluginSelection {
    fn new(memory: Option<bool>, usagebar: Option<bool>, usagebar_supported: bool) -> Result<Self> {
        ensure!(
            usagebar != Some(true) || usagebar_supported,
            "AI-UsageBar não está disponível nesta plataforma. Consulte {}.",
            crate::usagebar::PROJECT_URL,
        );
        Ok(Self {
            memory: memory.unwrap_or(true),
            usagebar: usagebar.unwrap_or(false) && usagebar_supported,
            memory_fixed: memory.is_some(),
            usagebar_fixed: usagebar.is_some(),
            usagebar_supported,
            focused: 0,
        })
    }

    fn key(&mut self, key: KeyEvent, columns: u16, rows: u16) -> Result<Option<PluginAction>> {
        if key.kind == KeyEventKind::Release {
            return Ok(None);
        }
        ensure!(
            !(key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)),
            "Seleção interrompida."
        );
        if key.code == KeyCode::Esc {
            return Ok(Some(PluginAction::Skip));
        }
        if columns < MIN_COLUMNS || rows < MIN_ROWS {
            return Ok(None);
        }
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab => {
                self.focused = if self.usagebar_supported {
                    1 - self.focused
                } else {
                    0
                };
            }
            KeyCode::Char(' ') if self.focused == 0 && !self.memory_fixed => {
                self.memory = !self.memory;
            }
            KeyCode::Char(' ') if self.focused == 1 && !self.usagebar_fixed => {
                self.usagebar = !self.usagebar;
            }
            KeyCode::Enter => {
                return Ok(Some(PluginAction::Continue(
                    self.memory,
                    self.usagebar && self.usagebar_supported,
                )));
            }
            _ => {}
        }
        Ok(None)
    }

    fn frame(&self, columns: u16, rows: u16) -> Frame {
        let width = usize::from(columns).min(84);
        let height = usize::from(rows).min(24);
        if columns < MIN_COLUMNS || rows < MIN_ROWS {
            let mut frame = Frame::blank(width, height);
            frame.put(0, 0, "STACKPULSE · COMPLEMENTOS OPCIONAIS", Tone::Accent);
            frame.put(0, 2, "Amplie o terminal para 56 × 20.", Tone::Warning);
            frame.put(0, 4, "Esc pular · Ctrl+C interromper", Tone::Muted);
            return frame;
        }
        let mut frame = Frame::new(width, height);
        frame.content(1, "✦ STACKPULSE", Tone::Accent);
        frame.content(2, "Setup / Complementos opcionais", Tone::Muted);
        frame.content(4, "Personalize sua instalação", Tone::Text);
        let options = [
            (Package::Memory, self.memory, self.memory_fixed),
            (Package::UsageBar, self.usagebar, self.usagebar_fixed),
        ];
        for (i, (package, selected, fixed)) in options
            .iter()
            .take(if self.usagebar_supported { 2 } else { 1 })
            .enumerate()
        {
            let tone = if self.focused == i {
                Tone::Selected
            } else {
                Tone::Text
            };
            if self.focused == i {
                frame.content(5 + i, &" ".repeat(width - 6), tone);
            }
            frame.content(
                5 + i,
                &format!(
                    "[{}] Instalar {}{}",
                    if *selected { "x" } else { " " },
                    package.name(),
                    if *fixed { " · flag" } else { "" },
                ),
                tone,
            );
        }
        let (package, _, fixed) = options[self.focused];
        frame.content(8, package.description(), Tone::Text);
        frame.content(9, package.scope(), Tone::Muted);
        frame.content(
            10,
            &match package {
                Package::Memory => "Instala o pacote oficial do AI-Memory.".into(),
                Package::UsageBar => crate::usagebar::platform_description(),
            },
            Tone::Muted,
        );
        frame.content(12, "Criado por Fabio Akita (AkitaOnRails)", Tone::Text);
        frame.content(13, package.url(), Tone::Accent);
        frame.content(14, CREATOR_URL, Tone::Muted);
        frame.content(
            15,
            if fixed {
                "Definido pela flag · Esc pular editáveis"
            } else {
                "Espaço marcar/desmarcar · Esc pular"
            },
            Tone::Muted,
        );
        frame.content(height - 3, "[ Continuar → ]", Tone::Accent);
        frame.content(
            height - 2,
            "↑↓ / Tab escolher · Enter seguir · Ctrl+C sair",
            Tone::Muted,
        );
        frame
    }
}

struct Selection {
    selected: bool,
    package: Package,
}

impl Selection {
    fn key(&mut self, key: KeyEvent, columns: u16, rows: u16) -> Result<Option<bool>> {
        if key.kind == KeyEventKind::Release {
            return Ok(None);
        }
        ensure!(
            !(key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)),
            "Seleção interrompida."
        );
        if key.code == KeyCode::Esc {
            return Ok(Some(false));
        }
        // Keep the full description and attribution visible before accepting a choice.
        if columns < MIN_COLUMNS || rows < MIN_ROWS {
            return Ok(None);
        }
        match key.code {
            KeyCode::Char(' ') => self.selected = !self.selected,
            KeyCode::Enter => return Ok(Some(self.selected)),
            _ => {}
        }
        Ok(None)
    }

    fn frame(&self, columns: u16, rows: u16) -> Frame {
        let width = usize::from(columns).min(84);
        let height = usize::from(rows).min(24);
        if columns < MIN_COLUMNS || rows < MIN_ROWS {
            let mut frame = Frame::blank(width, height);
            frame.put(0, 0, "STACKPULSE · COMPLEMENTOS OPCIONAIS", Tone::Accent);
            frame.put(0, 2, "Amplie o terminal para 56 × 20.", Tone::Warning);
            frame.put(0, 4, "Esc pular · Ctrl+C interromper", Tone::Muted);
            return frame;
        }

        let mut frame = Frame::new(width, height);
        frame.content(1, "✦ STACKPULSE", Tone::Accent);
        frame.content(2, "Setup / Complementos opcionais", Tone::Muted);
        frame.content(4, "Personalize sua instalação", Tone::Text);
        let checkbox = format!(
            "[{}] Instalar {}",
            if self.selected { "x" } else { " " },
            self.package.name(),
        );
        frame.content(
            5,
            &format!("{checkbox:<width$}", width = width - 6),
            Tone::Selected,
        );
        frame.content(6, self.package.description(), Tone::Text);
        if matches!(self.package, Package::UsageBar) {
            frame.content(7, &crate::usagebar::platform_description(), Tone::Muted);
        }

        frame.content(8, "CRIADO POR", Tone::Muted);
        frame.content(9, "Fabio Akita (AkitaOnRails)", Tone::Text);
        frame.content(10, self.package.url(), Tone::Accent);
        frame.content(11, CREATOR_URL, Tone::Muted);

        frame.content(
            13,
            &format!("Instala o pacote oficial do {}.", self.package.name()),
            Tone::Text,
        );
        frame.content(14, self.package.scope(), Tone::Muted);
        frame.content(
            height - 3,
            &format!(
                "[ Continuar {} {} → ]",
                if self.selected { "com" } else { "sem" },
                self.package.name(),
            ),
            Tone::Accent,
        );
        frame.content(
            height - 2,
            "Espaço marcar · Enter continuar · Esc pular",
            Tone::Muted,
        );
        frame
    }
}

struct Signals(Vec<signal_hook::SigId>);

impl Signals {
    fn register(interrupted: Arc<AtomicBool>) -> Result<Self> {
        let mut signals = Self(Vec::new());
        for signal in [
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGHUP,
        ] {
            signals
                .0
                .push(signal_hook::flag::register(signal, interrupted.clone())?);
        }
        Ok(signals)
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.0.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn confirmation_uses_default_and_checkbox_changes_only_on_space() {
        for default in [true, false] {
            let mut state = Selection {
                selected: default,
                package: Package::Memory,
            };
            assert_eq!(
                state.key(key(KeyCode::Enter), 80, 24).unwrap(),
                Some(default)
            );
            assert!(
                state
                    .key(key(KeyCode::Char(' ')), 80, 24)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                state.key(key(KeyCode::Enter), 80, 24).unwrap(),
                Some(!default)
            );
            let release = KeyEvent::new_with_kind(
                KeyCode::Char(' '),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            );
            assert!(state.key(release, 80, 24).unwrap().is_none());
            assert_eq!(state.selected, !default);
        }
    }

    #[test]
    fn resize_preserves_choice_and_blocks_confirmation_without_full_text() {
        let mut state = Selection {
            selected: true,
            package: Package::Memory,
        };
        state.key(key(KeyCode::Char(' ')), 56, 20).unwrap();
        for (columns, rows) in [(55, 24), (80, 19), (1, 1)] {
            let frame = state.frame(columns, rows);
            assert!(
                !frame
                    .spans
                    .iter()
                    .any(|span| span.text.contains("Continuar"))
            );
            assert_eq!(state.key(key(KeyCode::Enter), columns, rows).unwrap(), None);
            state.key(key(KeyCode::Char(' ')), columns, rows).unwrap();
            assert!(!state.selected);
        }
        assert_eq!(state.key(key(KeyCode::Enter), 56, 20).unwrap(), Some(false));
    }

    #[test]
    fn skip_and_interrupt_work_even_when_terminal_is_small() {
        let mut state = Selection {
            selected: true,
            package: Package::Memory,
        };
        assert_eq!(state.key(key(KeyCode::Esc), 1, 1).unwrap(), Some(false));
        assert!(
            state
                .key(
                    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                    1,
                    1
                )
                .is_err()
        );
    }

    #[test]
    fn minimum_frame_contains_full_attribution_and_package_scope() {
        let state = Selection {
            selected: true,
            package: Package::Memory,
        };
        let frame = state.frame(56, 20);
        for text in [
            "Fabio Akita (AkitaOnRails)",
            PROJECT_URL,
            CREATOR_URL,
            "Conexão aos agentes é uma etapa separada.",
            "[ Continuar com AI-Memory → ]",
        ] {
            assert!(frame.spans.iter().any(|span| span.text == text), "{text}");
        }
        assert!(
            frame
                .spans
                .iter()
                .any(|span| span.text.trim_end() == "[x] Instalar AI-Memory")
        );
        for (columns, rows) in [(1, 1), (55, 19), (56, 20), (84, 24), (120, 40)] {
            let frame = state.frame(columns, rows);
            for span in frame.spans {
                assert!(span.x + span.text.width() <= frame.width);
                assert!(span.y < frame.height);
            }
        }
    }

    #[test]
    fn usagebar_is_opt_in_with_attribution_and_graphical_scope_at_minimum_size() {
        let mut state = Selection {
            selected: false,
            package: Package::UsageBar,
        };
        assert_eq!(state.key(key(KeyCode::Enter), 56, 20).unwrap(), Some(false));
        for (columns, rows) in [(56, 20), (84, 24), (120, 40)] {
            let frame = state.frame(columns, rows);
            for text in [
                "Fabio Akita (AkitaOnRails)",
                crate::usagebar::PROJECT_URL,
                CREATOR_URL,
                "Consumo dos CLIs em texto ou painel TUI.",
                "Barra gráfica: configure separadamente.",
                "[ Continuar sem AI-UsageBar → ]",
            ] {
                assert!(frame.spans.iter().any(|span| span.text == text), "{text}");
            }
            assert!(
                frame
                    .spans
                    .iter()
                    .any(|span| span.text.trim_end() == "[ ] Instalar AI-UsageBar")
            );
            assert!(
                frame
                    .spans
                    .iter()
                    .any(|span| span.text == crate::usagebar::platform_description())
            );
            for span in frame.spans {
                assert!(span.x + span.text.width() <= frame.width);
                assert!(span.y < frame.height);
            }
        }
        state.key(key(KeyCode::Char(' ')), 56, 20).unwrap();
        assert_eq!(state.key(key(KeyCode::Enter), 56, 20).unwrap(), Some(true));
    }

    #[test]
    fn combined_plugins_share_one_page_with_independent_default_choices() {
        let mut state = PluginSelection::new(None, None, true).unwrap();
        assert_eq!(
            state.key(key(KeyCode::Enter), 56, 20).unwrap(),
            Some(PluginAction::Continue(true, false)),
        );
        let frame = state.frame(56, 20);
        for text in ["[x] Instalar AI-Memory", "[ ] Instalar AI-UsageBar"] {
            assert!(frame.spans.iter().any(|span| span.text == text));
        }
        assert_eq!(
            frame
                .spans
                .iter()
                .filter(|span| span.text.contains("Continuar"))
                .count(),
            1
        );
        state.key(key(KeyCode::Down), 56, 20).unwrap();
        state.key(key(KeyCode::Char(' ')), 56, 20).unwrap();
        assert_eq!((state.memory, state.usagebar), (true, true));
        state.key(key(KeyCode::Tab), 56, 20).unwrap();
        state.key(key(KeyCode::Char(' ')), 56, 20).unwrap();
        assert_eq!((state.memory, state.usagebar), (false, true));
        state.key(key(KeyCode::BackTab), 56, 20).unwrap();
        assert_eq!(state.focused, 1);
        state.key(key(KeyCode::Up), 56, 20).unwrap();
        assert_eq!(state.focused, 0);
        assert_eq!(
            state.key(key(KeyCode::Enter), 56, 20).unwrap(),
            Some(PluginAction::Continue(false, true)),
        );
    }

    #[test]
    fn combined_plugins_keep_explicit_flags_visible_and_fixed() {
        for memory in [false, true] {
            for usagebar in [false, true] {
                let mut state = PluginSelection::new(Some(memory), Some(usagebar), true).unwrap();
                for focus in 0..2 {
                    state.focused = focus;
                    state.key(key(KeyCode::Char(' ')), 56, 20).unwrap();
                    let frame = state.frame(56, 20);
                    assert!(
                        frame
                            .spans
                            .iter()
                            .any(|span| span.text == "Definido pela flag · Esc pular editáveis")
                    );
                    assert_eq!(
                        frame
                            .spans
                            .iter()
                            .filter(|span| span.text.ends_with(" · flag"))
                            .count(),
                        2
                    );
                }
                assert_eq!(
                    state.key(key(KeyCode::Enter), 56, 20).unwrap(),
                    Some(PluginAction::Continue(memory, usagebar)),
                );
                assert_eq!(
                    state.key(key(KeyCode::Esc), 56, 20).unwrap(),
                    Some(PluginAction::Skip)
                );
            }
        }
        let mut state = PluginSelection::new(Some(false), None, true).unwrap();
        state.key(key(KeyCode::Char(' ')), 56, 20).unwrap();
        state.key(key(KeyCode::Down), 56, 20).unwrap();
        state.key(key(KeyCode::Char(' ')), 56, 20).unwrap();
        assert_eq!((state.memory, state.usagebar), (false, true));
    }

    #[test]
    fn combined_plugins_hide_unsupported_usagebar_and_reject_explicit_install() {
        assert!(PluginSelection::new(None, Some(true), false).is_err());
        for flag in [None, Some(false)] {
            let mut state = PluginSelection::new(None, flag, false).unwrap();
            let frame = state.frame(56, 20);
            assert!(!frame.spans.iter().any(|span| span.text.contains("UsageBar") || span.text.contains("ai-usagebar")));
            for code in [KeyCode::Down, KeyCode::Tab, KeyCode::Up, KeyCode::BackTab] {
                state.key(key(code), 56, 20).unwrap();
                assert_eq!(state.focused, 0);
            }
            state.key(key(KeyCode::Char(' ')), 56, 20).unwrap();
            assert_eq!(
                state.key(key(KeyCode::Enter), 56, 20).unwrap(),
                Some(PluginAction::Continue(false, false)),
            );
        }
    }

    #[test]
    fn combined_plugins_preserve_resize_choices_and_only_confirm_when_visible() {
        let mut state = PluginSelection::new(None, None, true).unwrap();
        state.key(key(KeyCode::Down), 56, 20).unwrap();
        state.key(key(KeyCode::Char(' ')), 56, 20).unwrap();
        for (columns, rows) in [(1, 1), (55, 24), (56, 19)] {
            assert!(
                state
                    .key(key(KeyCode::Enter), columns, rows)
                    .unwrap()
                    .is_none()
            );
            state.key(key(KeyCode::Char(' ')), columns, rows).unwrap();
            state.key(key(KeyCode::Down), columns, rows).unwrap();
            assert_eq!(
                (state.memory, state.usagebar, state.focused),
                (true, true, 1)
            );
            assert!(
                !state
                    .frame(columns, rows)
                    .spans
                    .iter()
                    .any(|span| span.text.contains("Continuar"))
            );
        }
        let release = KeyEvent::new_with_kind(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        state.key(release, 56, 20).unwrap();
        assert!(state.usagebar);
        assert_eq!(
            state.key(key(KeyCode::Esc), 1, 1).unwrap(),
            Some(PluginAction::Skip)
        );
        assert!(
            state
                .key(
                    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                    1,
                    1
                )
                .is_err()
        );
        assert_eq!(
            state.key(key(KeyCode::Enter), 56, 20).unwrap(),
            Some(PluginAction::Continue(true, true))
        );
    }

    #[test]
    fn combined_plugins_show_focused_scope_and_credits_within_terminal_bounds() {
        let mut state = PluginSelection::new(None, None, true).unwrap();
        for focused in 0..2 {
            state.focused = focused;
            let package = if focused == 0 {
                Package::Memory
            } else {
                Package::UsageBar
            };
            for (columns, rows) in [(1, 1), (55, 19), (56, 20), (84, 24), (120, 40)] {
                let frame = state.frame(columns, rows);
                if columns >= MIN_COLUMNS && rows >= MIN_ROWS {
                    for text in [
                        package.description(),
                        package.scope(),
                        package.url(),
                        CREATOR_URL,
                        "Criado por Fabio Akita (AkitaOnRails)",
                    ] {
                        assert!(frame.spans.iter().any(|span| span.text == text), "{text}");
                    }
                }
                for span in frame.spans {
                    assert!(span.x + span.text.width() <= frame.width);
                    assert!(span.y < frame.height);
                }
            }
        }
    }
}
