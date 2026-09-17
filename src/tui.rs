//! Shared terminal surface, styling and Unicode-aware input for interactive commands.
use crate::widget::clean;
use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, KeyCode, KeyEvent, KeyModifiers},
    execute, queue,
    style::{
        Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
    terminal::{self, ClearType},
};
use std::{
    cell::RefCell,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};
use unicode_width::UnicodeWidthChar;

thread_local! {
    static DRAW_CACHE: RefCell<Option<RenderedFrame>> = const { RefCell::new(None) };
}

fn invalidate_draw_cache() {
    DRAW_CACHE.with(|cache| cache.borrow_mut().take());
}

pub fn available() -> bool {
    io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && std::env::var("TERM").as_deref() != Ok("dumb")
}

pub(crate) fn colors() -> bool {
    std::env::var_os("NO_COLOR").is_none()
}

pub(crate) fn display_path(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(relative) = path.strip_prefix(PathBuf::from(home))
    {
        return Path::new("~").join(relative).to_string_lossy().into_owned();
    }
    path.to_string_lossy().into_owned()
}

/// Click/release and wheel events using SGR coordinates, without motion reports.
struct EnableClickMouseCapture;

impl crossterm::Command for EnableClickMouseCapture {
    fn write_ansi(&self, output: &mut impl std::fmt::Write) -> std::fmt::Result {
        output.write_str("\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1000h\x1b[?1006h")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        crossterm::Command::execute_winapi(&event::EnableMouseCapture)
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        false
    }
}

pub(crate) struct TerminalGuard {
    mouse: bool,
    keyboard_enhanced: bool,
}
impl TerminalGuard {
    pub(crate) fn enter() -> Result<Self> {
        Self::enter_mode(false)
    }

    pub(crate) fn enter_with_mouse() -> Result<Self> {
        Self::enter_mode(true)
    }

    /// Keep this guard's original raw-mode snapshot and screen ownership while
    /// reasserting input reporting after a background command has finished.
    pub(crate) fn refresh_input(&self) -> Result<()> {
        refresh_input(&mut io::stdout(), self.mouse, self.keyboard_enhanced)?;
        Ok(())
    }

    pub(crate) fn set_mouse_capture(&mut self, enabled: bool) -> Result<()> {
        // Own cleanup before enabling: a partially failed write may already
        // have turned capture on. Failed disabling also keeps cleanup armed.
        self.mouse |= enabled;
        set_mouse_capture(&mut io::stdout(), enabled)?;
        self.mouse = enabled;
        Ok(())
    }

    fn enter_mode(mouse: bool) -> Result<Self> {
        invalidate_draw_cache();
        terminal::enable_raw_mode()?;
        // Own cleanup before writing any mode sequences: partially failed entry
        // must restore raw mode, screen state, and an opted-in mouse capture.
        let mut guard = Self {
            mouse,
            keyboard_enhanced: false,
        };
        execute!(
            io::stdout(),
            terminal::EnterAlternateScreen,
            cursor::Hide,
            terminal::DisableLineWrap,
            event::EnableBracketedPaste
        )?;
        #[cfg(unix)]
        {
            // Unsupported terminals ignore this request. Legacy Windows input
            // already reports modifiers and does not support this command.
            guard.keyboard_enhanced = true;
            execute!(
                io::stdout(),
                event::PushKeyboardEnhancementFlags(
                    event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                )
            )?;
        }
        set_mouse_capture(&mut io::stdout(), mouse)?;
        Ok(guard)
    }
}

fn set_mouse_capture(output: &mut impl Write, enabled: bool) -> io::Result<()> {
    if enabled {
        execute!(output, EnableClickMouseCapture)
    } else {
        execute!(output, event::DisableMouseCapture)
    }
}

fn refresh_input(output: &mut impl Write, mouse: bool, keyboard_enhanced: bool) -> io::Result<()> {
    execute!(output, event::EnableBracketedPaste)?;
    if keyboard_enhanced {
        // Set the current kitty flags; pushing here would leak one stack entry
        // each time a background request finishes. The guard owns one push/pop.
        output.write_all(b"\x1b[=1u")?;
    }
    set_mouse_capture(output, mouse)
}

fn restore_terminal(
    output: &mut impl Write,
    mouse: bool,
    keyboard_enhanced: bool,
) -> io::Result<()> {
    // Try mouse restoration independently, even if a later screen write fails.
    let mouse_result = if mouse {
        execute!(output, event::DisableMouseCapture)
    } else {
        Ok(())
    };
    // Restore the keyboard stack before leaving its alternate screen. Keep
    // screen cleanup independent if this write fails.
    let keyboard_result = if keyboard_enhanced {
        output.write_all(b"\x1b[<1u")
    } else {
        Ok(())
    };
    let screen_result = execute!(
        output,
        terminal::EndSynchronizedUpdate,
        ResetColor,
        SetAttribute(Attribute::Reset),
        event::DisableBracketedPaste,
        terminal::EnableLineWrap,
        cursor::Show,
        terminal::LeaveAlternateScreen
    );
    if mouse && mouse_result.is_err() {
        // A transient failed write must not strand capture while the caller
        // resumes another terminal UI. Raw-mode restoration still runs in Drop.
        let _ = execute!(output, event::DisableMouseCapture);
    }
    mouse_result.and(keyboard_result).and(screen_result)
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        invalidate_draw_cache();
        let _ = restore_terminal(&mut io::stdout(), self.mouse, self.keyboard_enhanced);
        let _ = terminal::disable_raw_mode();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Tone {
    Text,
    Muted,
    Accent,
    Selected,
    Warning,
    Navigation,
    Primary,
    Success,
    Danger,
    Highlight,
}
pub(crate) struct Span {
    pub(crate) x: usize,
    pub(crate) y: usize,
    pub(crate) text: String,
    pub(crate) tone: Tone,
}
pub(crate) struct Frame {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) spans: Vec<Span>,
    pub(crate) caret: Option<(usize, usize)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Cell {
    Empty,
    Glyph {
        text: String,
        width: usize,
        tone: Tone,
    },
    Continuation(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RenderedFrame {
    columns: u16,
    rows: u16,
    lines: Vec<Vec<Cell>>,
    caret: Option<(u16, u16)>,
    color: bool,
}

impl Frame {
    pub(crate) fn blank(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            spans: Vec::new(),
            caret: None,
        }
    }
    pub(crate) fn new(width: usize, height: usize) -> Self {
        let mut frame = Self::blank(width, height);
        if width < 2 || height < 2 {
            return frame;
        }
        frame.put(0, 0, &format!("╭{}╮", "─".repeat(width - 2)), Tone::Muted);
        frame.put(
            0,
            height - 1,
            &format!("╰{}╯", "─".repeat(width - 2)),
            Tone::Muted,
        );
        for y in 1..height - 1 {
            frame.put(0, y, "│", Tone::Muted);
            frame.put(width - 1, y, "│", Tone::Muted);
        }
        frame
    }
    pub(crate) fn put(&mut self, x: usize, y: usize, text: &str, tone: Tone) {
        if x < self.width && y < self.height {
            self.spans.push(Span {
                x,
                y,
                text: clean(text, self.width - x),
                tone,
            });
        }
    }
    pub(crate) fn content(&mut self, y: usize, text: &str, tone: Tone) {
        self.put(3, y, &clean(text, self.width.saturating_sub(6)), tone);
    }
    pub(crate) fn draw(&self, columns: u16, rows: u16, color: bool) -> Result<()> {
        let next = self.rasterize(columns, rows, color);
        let update = DRAW_CACHE.with(|cache| render_update(cache.borrow().as_ref(), &next))?;
        if update.is_empty() {
            return Ok(());
        }
        let mut out = io::stdout().lock();
        if let Err(error) = out.write_all(&update).and_then(|()| out.flush()) {
            invalidate_draw_cache();
            return Err(error.into());
        }
        DRAW_CACHE.with(|cache| cache.replace(Some(next)));
        Ok(())
    }

    fn rasterize(&self, columns: u16, rows: u16, color: bool) -> RenderedFrame {
        let columns_usize = usize::from(columns);
        let rows_usize = usize::from(rows);
        let left = columns_usize.saturating_sub(self.width) / 2;
        let top = rows_usize.saturating_sub(self.height) / 2;
        let mut lines = vec![vec![Cell::Empty; columns_usize]; rows_usize];

        for span in &self.spans {
            let Some(line) = lines.get_mut(top.saturating_add(span.y)) else {
                continue;
            };
            let mut x = left.saturating_add(span.x);
            if x >= columns_usize {
                continue;
            }
            let mut last_glyph = None;
            let mut leading_marks = String::new();
            for character in span
                .text
                .chars()
                .filter(|character| !character.is_control())
            {
                let width = character.width().unwrap_or(0);
                if width == 0 {
                    if let Some(index) = last_glyph
                        && let Cell::Glyph { text, .. } = &mut line[index]
                    {
                        text.push(character);
                    } else {
                        leading_marks.push(character);
                    }
                    continue;
                }
                if x.saturating_add(width) > columns_usize {
                    break;
                }
                clear_occupied(line, x, width);
                let mut text = std::mem::take(&mut leading_marks);
                text.push(character);
                line[x] = Cell::Glyph {
                    text,
                    width,
                    tone: span.tone,
                };
                for cell in line.iter_mut().skip(x + 1).take(width - 1) {
                    *cell = Cell::Continuation(x);
                }
                last_glyph = Some(x);
                x += width;
            }
        }

        let caret = self.caret.and_then(|(x, y)| {
            let x = left.saturating_add(x);
            let y = top.saturating_add(y);
            (x < columns_usize && y < rows_usize).then_some((x as u16, y as u16))
        });
        RenderedFrame {
            columns,
            rows,
            lines,
            caret,
            color,
        }
    }
}

fn clear_occupied(line: &mut [Cell], x: usize, width: usize) {
    let mut starts = Vec::with_capacity(width);
    for (offset, cell) in line.iter().skip(x).take(width).enumerate() {
        let start = match cell {
            Cell::Glyph { .. } => Some(x + offset),
            Cell::Continuation(start) => Some(*start),
            Cell::Empty => None,
        };
        if let Some(start) = start
            && !starts.contains(&start)
        {
            starts.push(start);
        }
    }
    for start in starts {
        let old_width = match &line[start] {
            Cell::Glyph { width, .. } => *width,
            _ => 1,
        };
        for cell in line.iter_mut().skip(start).take(old_width) {
            *cell = Cell::Empty;
        }
    }
}

fn render_update(previous: Option<&RenderedFrame>, next: &RenderedFrame) -> io::Result<Vec<u8>> {
    if previous == Some(next) {
        return Ok(Vec::new());
    }

    let resized = previous
        .is_none_or(|previous| previous.columns != next.columns || previous.rows != next.rows);
    let content_changed = resized
        || previous
            .is_none_or(|previous| previous.lines != next.lines || previous.color != next.color);
    let was_visible = previous.is_some_and(|previous| previous.caret.is_some());
    let mut out = Vec::new();
    queue!(out, terminal::BeginSynchronizedUpdate)?;
    if previous.is_none() || (was_visible && next.caret.is_none()) {
        queue!(out, cursor::Hide)?;
    }
    queue!(out, ResetColor, SetAttribute(Attribute::Reset))?;

    if resized {
        queue!(out, terminal::Clear(ClearType::All))?;
    }
    if content_changed {
        for (y, line) in next.lines.iter().enumerate() {
            let changed = resized
                || previous.is_none_or(|previous| {
                    previous.lines[y] != *line
                        || (previous.color != next.color
                            && line.iter().any(|cell| !matches!(cell, Cell::Empty)))
                });
            if !changed || (resized && line.iter().all(|cell| matches!(cell, Cell::Empty))) {
                continue;
            }
            draw_line(&mut out, y as u16, line, next.color)?;
        }
    }

    if let Some((x, y)) = next.caret {
        queue!(out, cursor::MoveTo(x, y))?;
        if !was_visible {
            queue!(out, cursor::Show)?;
        }
    }
    queue!(out, terminal::EndSynchronizedUpdate)?;
    Ok(out)
}

fn draw_line(out: &mut Vec<u8>, y: u16, line: &[Cell], color: bool) -> io::Result<()> {
    queue!(
        out,
        cursor::MoveTo(0, y),
        ResetColor,
        SetAttribute(Attribute::Reset)
    )?;
    let mut x = 0;
    while x < line.len() {
        match &line[x] {
            Cell::Empty => {
                let start = x;
                while matches!(line.get(x), Some(Cell::Empty)) {
                    x += 1;
                }
                queue!(out, Print(" ".repeat(x - start)))?;
            }
            Cell::Glyph { text, width, tone } => {
                let tone = *tone;
                let mut text = text.clone();
                x += width;
                while let Some(Cell::Glyph {
                    text: next_text,
                    width,
                    tone: next_tone,
                }) = line.get(x)
                {
                    if *next_tone != tone {
                        break;
                    }
                    text.push_str(next_text);
                    x += width;
                }
                set_tone(out, tone, color)?;
                queue!(out, Print(text), ResetColor, SetAttribute(Attribute::Reset))?;
            }
            // The lead glyph prints every terminal column occupied by a wide glyph.
            Cell::Continuation(_) => x += 1,
        }
    }
    Ok(())
}

fn set_tone(out: &mut Vec<u8>, tone: Tone, color: bool) -> io::Result<()> {
    if color {
        let foreground = match tone {
            Tone::Text => Color::Rgb {
                r: 235,
                g: 239,
                b: 245,
            },
            Tone::Muted => Color::Rgb {
                r: 160,
                g: 170,
                b: 184,
            },
            Tone::Accent => Color::Rgb {
                r: 94,
                g: 224,
                b: 203,
            },
            Tone::Navigation => Color::Rgb {
                r: 133,
                g: 183,
                b: 255,
            },
            Tone::Selected | Tone::Primary => Color::Rgb {
                r: 238,
                g: 246,
                b: 255,
            },
            Tone::Success => Color::Rgb {
                r: 119,
                g: 217,
                b: 167,
            },
            Tone::Danger => Color::Rgb {
                r: 255,
                g: 150,
                b: 168,
            },
            Tone::Highlight => Color::Rgb {
                r: 196,
                g: 173,
                b: 255,
            },
            Tone::Warning => Color::Rgb {
                r: 239,
                g: 190,
                b: 108,
            },
        };
        queue!(out, SetForegroundColor(foreground))?;
        let background = match tone {
            Tone::Selected => Some(Color::Rgb {
                r: 32,
                g: 57,
                b: 93,
            }),
            Tone::Primary => Some(Color::Rgb {
                r: 37,
                g: 99,
                b: 235,
            }),
            _ => None,
        };
        if let Some(background) = background {
            queue!(out, SetBackgroundColor(background))?;
        }
    }
    if matches!(
        tone,
        Tone::Accent
            | Tone::Selected
            | Tone::Navigation
            | Tone::Primary
            | Tone::Success
            | Tone::Danger
            | Tone::Highlight
    ) {
        queue!(out, SetAttribute(Attribute::Bold))?;
    }
    if !color && matches!(tone, Tone::Selected | Tone::Primary) {
        queue!(out, SetAttribute(Attribute::Reverse))?;
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Input {
    pub(crate) value: String,
    pub(crate) cursor: usize,
}
impl Input {
    pub(crate) fn new(value: String) -> Self {
        Self {
            cursor: value.len(),
            value,
        }
    }
    fn left(&mut self) {
        if let Some((i, _)) = self.value[..self.cursor].char_indices().next_back() {
            self.cursor = i;
        }
    }
    fn right(&mut self) {
        if let Some(c) = self.value[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }
    pub(crate) fn insert(&mut self, value: &str) {
        self.insert_chars(value.chars().filter(|c| !c.is_control()));
    }
    /// Keep pasted prompt line breaks while excluding terminal control bytes.
    pub(crate) fn insert_multiline(&mut self, value: &str) {
        let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
        self.insert_chars(
            normalized
                .chars()
                .filter(|c| matches!(*c, '\n' | '\t') || !c.is_control()),
        );
    }
    /// Reject oversized input atomically so a partial pasted prompt is never submitted.
    pub(crate) fn insert_checked(
        &mut self,
        value: &str,
        multiline: bool,
    ) -> std::result::Result<(), String> {
        let normalized = if multiline {
            value.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            value.to_owned()
        };
        let text: String = normalized
            .chars()
            .filter(|c| (multiline && matches!(*c, '\n' | '\t')) || !c.is_control())
            .collect();
        if text.chars().count() > 4096 {
            return Err("Cole até 4096 caracteres por vez. O campo foi preservado.".into());
        }
        if self.value.len().saturating_add(text.len()) > 8192 {
            return Err("O campo aceita até 8 KiB de texto. O conteúdo foi preservado.".into());
        }
        if multiline {
            self.insert_multiline(&text);
        } else {
            self.insert(&text);
        }
        Ok(())
    }
    fn insert_chars(&mut self, chars: impl Iterator<Item = char>) {
        for c in chars.take(4096) {
            if self.value.len() + c.len_utf8() > 8192 {
                break;
            }
            self.value.insert(self.cursor, c);
            self.cursor += c.len_utf8();
        }
    }
    pub(crate) fn key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(&c.to_string())
            }
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.len(),
            KeyCode::Backspace if self.cursor > 0 => {
                self.left();
                self.value.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.value.len() => {
                self.value.remove(self.cursor);
            }
            _ => {}
        }
    }
    pub(crate) fn visible(&self, width: usize) -> (String, usize) {
        if width == 0 {
            return (String::new(), 0);
        }
        let display_char = |c| match c {
            '\n' => Some('↵'),
            '\t' => Some('⇥'),
            c if !c.is_control() => Some(c),
            _ => None,
        };
        let display: String = self.value.chars().filter_map(display_char).collect();
        let mut start = 0;
        let mut offset: usize = self.value[..self.cursor]
            .chars()
            .filter_map(display_char)
            .map(|c| c.width().unwrap_or(0))
            .sum();
        while offset >= width {
            let c = display[start..].chars().next().unwrap();
            start += c.len_utf8();
            offset = offset.saturating_sub(c.width().unwrap_or(0));
        }
        (clean(&display[start..], width), offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    #[cfg(unix)]
    fn mouse_capture_is_opt_in_sgr_without_motion_and_has_matching_cleanup() {
        let mut enabled = String::new();
        crossterm::Command::write_ansi(&EnableClickMouseCapture, &mut enabled).unwrap();
        assert!(enabled.contains("\x1b[?1000h"));
        assert!(enabled.contains("\x1b[?1006h"));
        assert!(!enabled.contains("\x1b[?1002h"));
        assert!(!enabled.contains("\x1b[?1003h"));
        let mut disabled = Vec::new();
        restore_terminal(&mut disabled, true, false).unwrap();
        assert!(contains(&disabled, b"\x1b[?1000l"));
        assert!(contains(&disabled, b"\x1b[?1006l"));
        assert!(contains(&disabled, b"\x1b[?1049l"));
        let mut plain = Vec::new();
        restore_terminal(&mut plain, false, false).unwrap();
        assert!(!contains(&plain, b"?1000"));
        assert!(!contains(&plain, b"?1006"));
    }

    #[test]
    #[cfg(unix)]
    fn terminal_cleanup_retries_mouse_after_a_failed_write_and_still_restores_screen() {
        struct FailOnce {
            failed: bool,
            bytes: Vec<u8>,
        }
        impl Write for FailOnce {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if !self.failed {
                    self.failed = true;
                    return Err(io::Error::other("transient write failure"));
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut output = FailOnce {
            failed: false,
            bytes: Vec::new(),
        };
        assert!(restore_terminal(&mut output, true, false).is_err());
        assert!(contains(&output.bytes, b"\x1b[?1049l"));
        assert!(contains(&output.bytes, b"\x1b[?1000l"));
        assert!(contains(&output.bytes, b"\x1b[?1006l"));
    }

    #[test]
    #[cfg(unix)]
    fn input_refresh_preserves_native_selection_and_does_not_grow_keyboard_stack() {
        let mut output = Vec::new();
        refresh_input(&mut output, false, true).unwrap();
        refresh_input(&mut output, false, true).unwrap();
        assert!(contains(&output, b"\x1b[?1000l"));
        assert!(!contains(&output, b"\x1b[?1000h"));
        assert!(contains(&output, b"\x1b[=1u"));
        assert!(!contains(&output, b"\x1b[>"));
        assert!(!contains(&output, b"\x1b[<"));
        output.clear();
        refresh_input(&mut output, true, true).unwrap();
        assert!(contains(&output, b"\x1b[?1000h"));
        output.clear();
        restore_terminal(&mut output, false, true).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert_eq!(text.matches("\x1b[<1u").count(), 1);
        assert!(text.find("\x1b[<1u").unwrap() < text.find("\x1b[?1049l").unwrap());
    }

    fn clear_bytes(kind: ClearType) -> Vec<u8> {
        let mut bytes = Vec::new();
        queue!(bytes, terminal::Clear(kind)).unwrap();
        bytes
    }

    fn cursor_bytes(show: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        if show {
            queue!(bytes, cursor::Show).unwrap();
        } else {
            queue!(bytes, cursor::Hide).unwrap();
        }
        bytes
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn multiline_paste_preserves_prompt_lines_without_terminal_controls() {
        let mut input = Input::new("ação".into());
        input.insert_multiline("\r\n界\r🙂\t\u{1b}");
        assert_eq!(input.value, "ação\n界\n🙂\t");
        assert_eq!(input.visible(20), ("ação↵界↵🙂⇥".into(), 11));
        input.key(key(KeyCode::Home));
        for _ in 0..5 {
            input.key(key(KeyCode::Right));
        }
        assert_eq!(input.visible(20), ("ação↵界↵🙂⇥".into(), 5));
        input.key(key(KeyCode::Backspace));
        assert_eq!(input.value, "ação界\n🙂\t");
        input.insert_multiline("\n");
        assert_eq!(input.value, "ação\n界\n🙂\t");
        assert_eq!(input.visible(20).1, 5);
    }

    #[test]
    fn multiline_caret_and_display_remain_bounded_when_scrolling() {
        let mut input = Input::new("ação\n界\n🙂\t".into());
        for _ in 0..=input.value.chars().count() {
            for width in 0..20 {
                let (display, caret) = input.visible(width);
                assert!(display.width() <= width);
                assert!(width == 0 || caret < width);
                assert!(!display.chars().any(char::is_control));
            }
            input.key(key(KeyCode::Left));
        }
        assert_eq!(Input::new(String::new()).visible(0), (String::new(), 0));
    }

    #[test]
    fn regular_inputs_still_filter_line_breaks_and_pastes_keep_utf8_boundaries() {
        let mut input = Input::new(String::new());
        input.insert("a\n\rb\t\u{1b}c");
        assert_eq!(input.value, "abc");
        input.insert_multiline(&"界\n".repeat(4096));
        assert!(input.value.len() <= 8192);
        assert_eq!(input.cursor, input.value.len());
        assert!(input.value.is_char_boundary(input.cursor));
    }

    #[test]
    fn checked_paste_rejects_oversized_text_without_changing_value_or_cursor() {
        let mut input = Input::new("ação\noriginal".into());
        input.key(key(KeyCode::Home));
        input.key(key(KeyCode::Right));
        let value = input.value.clone();
        let cursor = input.cursor;
        for paste in ["a".repeat(4097), "界".repeat(4096)] {
            assert!(input.insert_checked(&paste, true).is_err());
            assert_eq!(input.value, value);
            assert_eq!(input.cursor, cursor);
        }
        input.insert_checked("\r\n界\t\u{1b}", true).unwrap();
        assert_eq!(input.value, "a\n界\tção\noriginal");
        assert_eq!(input.cursor, "a\n界\t".len());
        let mut full = Input::new("x".repeat(8190));
        full.insert_checked("á", false).unwrap();
        assert_eq!(full.value.len(), 8192);
        assert!(full.insert_checked("z", false).is_err());
        assert_eq!(full.value.len(), 8192);
        assert_eq!(full.cursor, 8192);
        let mut plain = Input::new(String::new());
        plain.insert_checked("a\r\nb\t", false).unwrap();
        assert_eq!(plain.value, "ab");
    }

    #[test]
    fn tiny_frames_and_untrusted_content_are_clipped_safely() {
        for width in 0..12 {
            for height in 0..6 {
                let mut frame = Frame::new(width, height);
                frame.content(1, "long\nlabel\u{1b}[2J", Tone::Text);
                frame.put(0, 0, "界界\ntext", Tone::Accent);
                for span in frame.spans {
                    assert!(span.x < width && span.y < height);
                    assert!(span.x + span.text.width() <= width);
                    assert!(!span.text.chars().any(char::is_control));
                }
            }
        }
    }

    #[test]
    fn rasterization_tracks_wide_glyphs_and_clears_overlapped_cells() {
        let mut frame = Frame::blank(6, 1);
        frame.put(0, 0, "界x", Tone::Text);
        let rendered = frame.rasterize(6, 1, true);
        assert!(matches!(
            &rendered.lines[0][0],
            Cell::Glyph { text, width: 2, .. } if text == "界"
        ));
        assert_eq!(rendered.lines[0][1], Cell::Continuation(0));
        assert!(matches!(
            &rendered.lines[0][2],
            Cell::Glyph { text, width: 1, .. } if text == "x"
        ));

        let mut continuation_overlap = Frame::blank(6, 1);
        continuation_overlap.put(0, 0, "界", Tone::Text);
        continuation_overlap.put(1, 0, "a", Tone::Accent);
        let rendered = continuation_overlap.rasterize(6, 1, true);
        assert_eq!(rendered.lines[0][0], Cell::Empty);
        assert!(matches!(
            &rendered.lines[0][1],
            Cell::Glyph { text, width: 1, tone: Tone::Accent } if text == "a"
        ));

        // The overwritten wide glyph begins after an empty cell. This catches a
        // wrong lead index when scanning a mix of empty and occupied cells.
        let mut empty_then_wide = Frame::blank(6, 1);
        empty_then_wide.put(2, 0, "界", Tone::Text);
        empty_then_wide.put(1, 0, "好", Tone::Warning);
        let rendered = empty_then_wide.rasterize(6, 1, true);
        assert_eq!(rendered.lines[0][0], Cell::Empty);
        assert!(matches!(
            &rendered.lines[0][1],
            Cell::Glyph { text, width: 2, tone: Tone::Warning } if text == "好"
        ));
        assert_eq!(rendered.lines[0][2], Cell::Continuation(1));
        assert_eq!(rendered.lines[0][3], Cell::Empty);
    }

    #[test]
    fn incremental_updates_skip_identical_frames_and_erase_shortened_lines() {
        let mut long = Frame::blank(8, 2);
        long.put(0, 0, "abcdefgh", Tone::Text);
        let long = long.rasterize(8, 2, true);
        let initial = render_update(None, &long).unwrap();
        assert!(contains(&initial, &clear_bytes(ClearType::All)));
        assert!(render_update(Some(&long), &long).unwrap().is_empty());

        let mut short = Frame::blank(8, 2);
        short.put(0, 0, "abc", Tone::Text);
        let short = short.rasterize(8, 2, true);
        let update = render_update(Some(&long), &short).unwrap();
        assert!(!contains(&update, &clear_bytes(ClearType::All)));
        assert!(!contains(&update, &clear_bytes(ClearType::CurrentLine)));
        assert!(contains(&update, b"abc"));
        assert!(contains(&update, b"     "));

        let resized = Frame::blank(8, 2).rasterize(10, 3, true);
        let update = render_update(Some(&short), &resized).unwrap();
        assert!(contains(&update, &clear_bytes(ClearType::All)));
    }

    #[test]
    fn incremental_updates_preserve_styles_and_stable_caret_visibility() {
        let mut first = Frame::blank(8, 2);
        first.put(0, 0, "one", Tone::Selected);
        first.caret = Some((3, 1));
        let first = first.rasterize(8, 2, true);
        let initial = render_update(None, &first).unwrap();
        assert!(contains(&initial, &cursor_bytes(false)));
        assert!(contains(&initial, &cursor_bytes(true)));

        let mut foreground = Vec::new();
        queue!(
            foreground,
            SetForegroundColor(Color::Rgb {
                r: 238,
                g: 246,
                b: 255
            }),
            SetBackgroundColor(Color::Rgb {
                r: 32,
                g: 57,
                b: 93
            }),
            SetAttribute(Attribute::Bold)
        )
        .unwrap();
        assert!(contains(&initial, &foreground));

        let mut changed = Frame::blank(8, 2);
        changed.put(0, 0, "two", Tone::Selected);
        changed.caret = Some((3, 1));
        let changed = changed.rasterize(8, 2, true);
        let update = render_update(Some(&first), &changed).unwrap();
        assert!(!contains(&update, &cursor_bytes(false)));
        assert!(!contains(&update, &cursor_bytes(true)));

        let mut hidden = Frame::blank(8, 2);
        hidden.put(0, 0, "two", Tone::Selected);
        let hidden = hidden.rasterize(8, 2, true);
        let update = render_update(Some(&changed), &hidden).unwrap();
        assert!(contains(&update, &cursor_bytes(false)));
        assert!(!contains(&update, &cursor_bytes(true)));
    }

    #[test]
    fn draw_cache_can_be_invalidated_between_terminal_sessions() {
        let rendered = Frame::blank(2, 1).rasterize(2, 1, false);
        DRAW_CACHE.with(|cache| cache.replace(Some(rendered)));
        invalidate_draw_cache();
        DRAW_CACHE.with(|cache| assert!(cache.borrow().is_none()));
    }

    #[test]
    fn semantic_palette_preserves_contrast_and_no_color_selection() {
        let tones = [
            Tone::Text,
            Tone::Muted,
            Tone::Accent,
            Tone::Selected,
            Tone::Warning,
            Tone::Navigation,
            Tone::Primary,
            Tone::Success,
            Tone::Danger,
            Tone::Highlight,
        ];
        for tone in tones {
            let mut output = Vec::new();
            set_tone(&mut output, tone, false).unwrap();
            assert!(
                !contains(&output, b"38;"),
                "{tone:?} sets a foreground with NO_COLOR"
            );
            assert!(
                !contains(&output, b"48;"),
                "{tone:?} sets a background with NO_COLOR"
            );
            if matches!(tone, Tone::Selected | Tone::Primary) {
                let mut reverse = Vec::new();
                queue!(reverse, SetAttribute(Attribute::Reverse)).unwrap();
                assert!(contains(&output, &reverse));
            }
        }
        for (tone, rgb) in [
            (Tone::Navigation, (133, 183, 255)),
            (Tone::Success, (119, 217, 167)),
            (Tone::Danger, (255, 150, 168)),
            (Tone::Highlight, (196, 173, 255)),
        ] {
            let mut expected = Vec::new();
            queue!(
                expected,
                SetForegroundColor(Color::Rgb {
                    r: rgb.0,
                    g: rgb.1,
                    b: rgb.2
                })
            )
            .unwrap();
            let mut output = Vec::new();
            set_tone(&mut output, tone, true).unwrap();
            assert!(contains(&output, &expected), "{tone:?}");
        }
    }
}
