//! Explicit, session-only consent for the directory from which StackPulse was started.
use crate::tui::{self, Frame, TerminalGuard, Tone};
use anyhow::{Context, Result, ensure};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal,
};
use std::{
    io::{self, BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use unicode_width::UnicodeWidthChar;

const MIN_COLUMNS: u16 = 64;
const MIN_ROWS: u16 = 20;

#[derive(Clone, Debug)]
pub struct Workspace {
    pub path: PathBuf,
}

impl Workspace {
    /// Capture only the process directory. No project content or configuration is read.
    pub fn current() -> Result<Self> {
        let path = std::env::current_dir()
            .context("Não foi possível identificar a pasta atual")?
            .canonicalize()
            .context("Não foi possível resolver o caminho da pasta atual")?;
        Ok(Self { path })
    }

    /// An explicit path grants this invocation access only to the exact current directory.
    pub fn authorize(&self, supplied: Option<&Path>) -> Result<bool> {
        if let Some(path) = supplied {
            let approved = path.canonicalize().with_context(|| {
                format!(
                    "Não foi possível resolver --allow-workspace {}",
                    quoted_path(path)
                )
            })?;
            ensure!(
                approved == self.path,
                "--allow-workspace deve corresponder à pasta atual exata: {}",
                quoted_path(&self.path)
            );
            return Ok(true);
        }
        ensure!(
            io::stdin().is_terminal() && io::stdout().is_terminal(),
            "Autorize explicitamente a pasta atual em uso sem terminal interativo: stackpulse --allow-workspace \"$PWD\" ..."
        );
        if std::env::var("TERM").as_deref() == Ok("dumb") {
            return self.authorize_plain();
        }
        self.authorize_terminal()
    }

    fn authorize_plain(&self) -> Result<bool> {
        let mut out = io::stdout().lock();
        writeln!(out, "\nSTACKPULSE\n\nPermitir acesso à pasta?")?;
        writeln!(out, "{}\n", quoted_path(&self.path))?;
        writeln!(out, "Permite ler arquivos e executar CLIs nesta pasta.")?;
        writeln!(out, "As edições seguem as permissões da execução.")?;
        writeln!(out, "A escolha vale só nesta sessão.")?;
        write!(out, "Permitir nesta sessão? [s/N] ")?;
        out.flush()?;
        let allowed = read_answer(io::stdin().lock())?;
        writeln!(out)?;
        Ok(allowed)
    }

    fn authorize_terminal(&self) -> Result<bool> {
        let interrupted = Arc::new(AtomicBool::new(false));
        let mut signals = Signals(Vec::new());
        for signal in [
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGHUP,
        ] {
            signals
                .0
                .push(signal_hook::flag::register(signal, interrupted.clone())?);
        }
        let _terminal = TerminalGuard::enter()?;
        let mut state = Consent::default();
        let path = quoted_path(&self.path);
        loop {
            if interrupted.load(Ordering::Relaxed) {
                return Ok(false);
            }
            let (columns, rows) = terminal::size()?;
            consent_frame(&mut state, &path, columns, rows).draw(columns, rows, tui::colors())?;
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        if let Some(allowed) = state.key(key, columns, rows) {
                            return Ok(allowed);
                        }
                    }
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
        }
    }
}

/// Debug formatting escapes control characters and preserves non-UTF-8 path bytes.
fn quoted_path(path: &Path) -> String {
    format!("{:?}", path.as_os_str())
}

fn read_answer(reader: impl BufRead) -> Result<bool> {
    let mut answer = Vec::new();
    reader.take(257).read_until(b'\n', &mut answer)?;
    if answer.len() > 256 || answer.last() != Some(&b'\n') {
        return Ok(false);
    }
    let Ok(answer) = std::str::from_utf8(&answer) else {
        return Ok(false);
    };
    Ok(matches!(answer.trim().to_lowercase().as_str(), "s" | "sim"))
}

#[derive(Default)]
struct Consent {
    allow: bool,
    scroll: usize,
    page: usize,
    max_scroll: usize,
}
impl Consent {
    fn key(&mut self, key: KeyEvent, columns: u16, rows: u16) -> Option<bool> {
        if key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return Some(false);
        }
        // Do not approve while the full consent text cannot be shown.
        if columns < MIN_COLUMNS || rows < MIN_ROWS {
            return None;
        }
        match key.code {
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                self.allow = !self.allow;
            }
            KeyCode::Enter => return Some(self.allow),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(self.page),
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(self.page).min(self.max_scroll);
            }
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = self.max_scroll,
            _ => {}
        }
        None
    }
}

fn consent_frame(state: &mut Consent, path: &str, columns: u16, rows: u16) -> Frame {
    let width = usize::from(columns).min(100);
    let height = usize::from(rows).min(30);
    let mut frame = Frame::blank(width, height);
    if columns < MIN_COLUMNS || rows < MIN_ROWS {
        frame.put(0, 0, "STACKPULSE", Tone::Accent);
        frame.put(0, 2, "Amplie o terminal para 64 × 20.", Tone::Warning);
        frame.put(
            0,
            4,
            "Esc ou Ctrl+C cancela sem acessar a pasta.",
            Tone::Muted,
        );
        return frame;
    }
    frame.content(1, "STACKPULSE", Tone::Accent);
    frame.content(3, "Permitir acesso à pasta?", Tone::Text);
    frame.content(5, "PASTA ATUAL", Tone::Muted);
    let lines = wrap(path, width - 6);
    state.page = height - 17;
    state.max_scroll = lines.len().saturating_sub(state.page);
    state.scroll = state.scroll.min(state.max_scroll);
    for (row, line) in lines.iter().skip(state.scroll).take(state.page).enumerate() {
        frame.content(6 + row, line, Tone::Accent);
    }
    if state.max_scroll > 0 {
        frame.content(
            height - 11,
            &format!(
                "Caminho: {}–{}/{} · PgUp/PgDn para rolar",
                state.scroll + 1,
                (state.scroll + state.page).min(lines.len()),
                lines.len()
            ),
            Tone::Muted,
        );
    }
    frame.content(
        height - 9,
        "Permite ler arquivos e executar CLIs nesta pasta.",
        Tone::Text,
    );
    frame.content(
        height - 8,
        "As edições seguem as permissões da execução.",
        Tone::Text,
    );
    frame.content(height - 7, "A escolha vale só nesta sessão.", Tone::Muted);
    for (allow, row, label) in [
        (true, height - 5, "Permitir nesta sessão"),
        (false, height - 4, "Cancelar"),
    ] {
        let selected = state.allow == allow;
        frame.content(
            row,
            &format!("{} {label}", if selected { "›" } else { " " }),
            if selected { Tone::Selected } else { Tone::Text },
        );
    }
    frame.content(
        height - 2,
        "↑↓/Tab escolher · Enter confirmar · Esc sair",
        Tone::Muted,
    );
    frame
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut columns = 0;
    for ch in text.chars() {
        let size = UnicodeWidthChar::width(ch).unwrap_or(0);
        if columns + size > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            columns = 0;
        }
        line.push(ch);
        columns += size;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

struct Signals(Vec<signal_hook::SigId>);
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

    fn workspace(path: &Path) -> Workspace {
        Workspace {
            path: path.canonicalize().unwrap(),
        }
    }

    #[test]
    fn current_captures_the_callers_directory() {
        assert_eq!(
            Workspace::current().unwrap().path,
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
    }

    #[test]
    fn exact_explicit_permission_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        assert!(workspace(dir.path()).authorize(Some(dir.path())).unwrap());
    }

    #[test]
    fn parent_or_child_permission_does_not_authorize_current_directory() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        std::fs::create_dir(&child).unwrap();
        assert!(workspace(&child).authorize(Some(dir.path())).is_err());
        assert!(workspace(dir.path()).authorize(Some(&child)).is_err());
        assert!(
            workspace(dir.path())
                .authorize(Some(&dir.path().join("missing")))
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_permission_resolves_to_the_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original");
        let alias = dir.path().join("alias");
        std::fs::create_dir(&original).unwrap();
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        assert!(workspace(&original).authorize(Some(&alias)).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn displayed_path_escapes_controls_and_preserves_invalid_utf8() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let path = PathBuf::from(OsString::from_vec(b"/tmp/a\n\x1b[31m\xff".to_vec()));
        let display = quoted_path(&path);
        assert!(!display.chars().any(char::is_control));
        assert!(display.contains("\\n"));
        assert!(display.contains("\\xFF"));
        assert!(!display.contains('\u{fffd}'));
    }

    #[test]
    fn displayed_path_exposes_invisible_direction_and_spacing_characters() {
        let path = Path::new("/tmp/real\u{202e}name\u{2066}\u{200b}\u{feff}");
        let display = quoted_path(path);
        for ch in ['\u{202e}', '\u{2066}', '\u{200b}', '\u{feff}'] {
            assert!(!display.contains(ch));
            assert!(display.contains(&format!("\\u{{{:x}}}", u32::from(ch))));
        }
    }

    #[test]
    fn plain_prompt_accepts_only_one_explicit_answer_and_denies_eof() {
        for answer in ["s\n", "S\r\n", "sim\n"] {
            assert!(read_answer(answer.as_bytes()).unwrap());
        }
        for answer in ["", "s", "\n", "n\n", "whatever\n", "\ns\n"] {
            assert!(!read_answer(answer.as_bytes()).unwrap());
        }
        assert!(!read_answer(format!("{}\n", "s".repeat(300)).as_bytes()).unwrap());
    }

    #[test]
    fn default_denies_and_a_small_terminal_cannot_approve() {
        let mut state = Consent::default();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(state.key(enter, 80, 24), Some(false));
        state.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 80, 24);
        assert_eq!(state.key(enter, 40, 10), None);
        assert!(state.allow);
        assert_eq!(state.key(enter, 80, 24), Some(true));
        assert_eq!(
            state.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 1, 1),
            Some(false)
        );
    }

    #[test]
    fn long_paths_can_be_seen_completely_and_resize_preserves_selection() {
        let path = quoted_path(Path::new(&format!("/{}", "pasta-東京/".repeat(100))));
        let lines = wrap(&path, 58);
        assert_eq!(lines.concat(), path);
        let mut state = Consent {
            allow: true,
            ..Consent::default()
        };
        let frame = consent_frame(&mut state, &path, 64, 20);
        assert!(frame.spans.iter().any(|s| s.text.contains("PgUp/PgDn")));
        let mut displayed = Vec::new();
        loop {
            let frame = consent_frame(&mut state, &path, 64, 20);
            displayed.extend(
                frame
                    .spans
                    .into_iter()
                    .filter(|s| (6..9).contains(&s.y))
                    .map(|s| s.text),
            );
            if state.scroll == state.max_scroll {
                break;
            }
            state.key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE), 64, 20);
        }
        for line in lines {
            assert!(displayed.contains(&line));
        }
        let frame = consent_frame(&mut state, &path, 40, 10);
        assert!(
            !frame
                .spans
                .iter()
                .any(|s| s.text.contains("Permitir nesta sessão"))
        );
        consent_frame(&mut state, &path, 100, 30);
        assert!(state.allow);
        assert!(state.scroll <= state.max_scroll);
    }
}
