//! Multiline prompt editing without silently shortening pasted requests.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

const MAX_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(crate) struct Composer {
    pub(crate) value: String,
    /// UTF-8 byte offset of the insertion point.
    pub(crate) cursor: usize,
}

impl Composer {
    /// An oversized paste leaves both the text and insertion point unchanged.
    pub(crate) fn insert(&mut self, value: &str) -> Result<(), String> {
        let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
        let text: String = normalized
            .chars()
            .filter(|c| matches!(*c, '\n' | '\t') || !c.is_control())
            .collect();
        if self.value.len().saturating_add(text.len()) > MAX_BYTES {
            return Err("O pedido pode ter até 64 KiB. O texto foi preservado.".into());
        }
        self.cursor = self.boundary();
        self.value.insert_str(self.cursor, &text);
        self.cursor += text.len();
        Ok(())
    }

    pub(crate) fn key(&mut self, key: KeyEvent) -> Result<(), String> {
        self.cursor = self.boundary();
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('u' | 'U') => {
                    self.value.clear();
                    self.cursor = 0;
                }
                KeyCode::Char('a' | 'A') => self.cursor = self.line_bounds().0,
                KeyCode::Char('e' | 'E') => self.cursor = self.line_bounds().1,
                _ => {}
            }
            return Ok(());
        }
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                return self.insert(c.encode_utf8(&mut [0; 4]));
            }
            KeyCode::Left => {
                self.cursor = self.value[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(index, _)| index);
            }
            KeyCode::Right => {
                if let Some(c) = self.value[self.cursor..].chars().next() {
                    self.cursor += c.len_utf8();
                }
            }
            KeyCode::Home => self.cursor = self.line_bounds().0,
            KeyCode::End => self.cursor = self.line_bounds().1,
            KeyCode::Backspace => {
                if let Some((index, _)) = self.value[..self.cursor].char_indices().next_back() {
                    self.value.drain(index..self.cursor);
                    self.cursor = index;
                }
            }
            KeyCode::Delete => {
                if let Some(c) = self.value[self.cursor..].chars().next() {
                    self.value.drain(self.cursor..self.cursor + c.len_utf8());
                }
            }
            KeyCode::Up => self.vertical(false),
            KeyCode::Down => self.vertical(true),
            _ => {}
        }
        Ok(())
    }

    /// Wrap text into terminal cells and return the caret as (column, row).
    /// A full final row gets an empty continuation row for its insertion point.
    pub(crate) fn lines(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        self.layout(width, |_, _, _| {})
    }

    /// Position the insertion point from a click relative to `lines(width)`.
    /// The caller accounts for viewport scrolling and screen offsets. Wide
    /// glyphs/tabs snap to the nearest byte boundary; ties prefer the left edge,
    /// then the end of combining marks at that same visual position.
    pub(crate) fn place_cursor(&mut self, column: usize, row: usize, width: usize) {
        if width == 0 {
            self.cursor = 0;
            return;
        }
        let mut best = None;
        let mut last_row = 0;
        self.layout(width, |index, col, visual_row| {
            last_row = last_row.max(visual_row);
            let score = (
                visual_row.abs_diff(row),
                col.abs_diff(column),
                col > column,
                std::cmp::Reverse(index),
            );
            if best.as_ref().is_none_or(|(previous, _)| score < *previous) {
                best = Some((score, index));
            }
        });
        self.cursor = if row > last_row {
            self.value.len()
        } else {
            best.map_or(0, |(_, index)| index)
        };
    }

    fn layout(
        &self,
        width: usize,
        mut visit: impl FnMut(usize, usize, usize),
    ) -> (Vec<String>, (usize, usize)) {
        if width == 0 {
            return (vec![String::new()], (0, 0));
        }
        let cursor = self.boundary();
        let mut lines = vec![String::new()];
        let (mut col, mut row) = (0, 0);
        let mut caret = None;
        for (index, c) in self.value.char_indices() {
            // Keep the end of a wrapped visual row clickable even when the
            // canonical caret for this byte belongs to the following row.
            visit(index, col, row);
            if col == width {
                visit(index, 0, row + 1);
            }
            match c {
                '\n' => {
                    if index == cursor {
                        caret = Some(caret_cell(col, row, width));
                    }
                    lines.push(String::new());
                    row += 1;
                    col = 0;
                }
                '\t' => {
                    if index == cursor {
                        caret = Some(caret_cell(col, row, width));
                    }
                    for cell in 0..tab_width(col) {
                        if col == width {
                            if cell > 0 {
                                visit(index + c.len_utf8(), col, row);
                            }
                            lines.push(String::new());
                            row += 1;
                            col = 0;
                        }
                        lines[row].push(' ');
                        col += 1;
                    }
                }
                c if c.is_control() => {
                    if index == cursor {
                        caret = Some(caret_cell(col, row, width));
                    }
                }
                c => {
                    let char_width = c.width().unwrap_or(0);
                    // A two-cell glyph cannot fit a one-cell viewport.
                    let (shown, cells) = if char_width > width {
                        ('�', 1)
                    } else {
                        (c, char_width)
                    };
                    if cells > 0 && col + cells > width {
                        lines.push(String::new());
                        row += 1;
                        col = 0;
                        visit(index, col, row);
                    }
                    if index == cursor {
                        caret = Some(caret_cell(col, row, width));
                    }
                    lines[row].push(shown);
                    col += cells;
                }
            }
        }
        visit(self.value.len(), col, row);
        if col == width {
            visit(self.value.len(), 0, row + 1);
        }
        let caret = caret.unwrap_or_else(|| caret_cell(col, row, width));
        while lines.len() <= caret.1 {
            lines.push(String::new());
        }
        (lines, caret)
    }

    fn boundary(&self) -> usize {
        let mut cursor = self.cursor.min(self.value.len());
        while !self.value.is_char_boundary(cursor) {
            cursor -= 1;
        }
        cursor
    }

    fn line_bounds(&self) -> (usize, usize) {
        let start = self.value[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
        let end = self.value[self.cursor..]
            .find('\n')
            .map_or(self.value.len(), |i| self.cursor + i);
        (start, end)
    }

    fn vertical(&mut self, down: bool) {
        let (start, end) = self.line_bounds();
        let (next_start, next_end) = if down {
            if end == self.value.len() {
                return;
            }
            let next_start = end + 1;
            let next_end = self.value[next_start..]
                .find('\n')
                .map_or(self.value.len(), |i| next_start + i);
            (next_start, next_end)
        } else {
            if start == 0 {
                return;
            }
            let next_end = start - 1;
            let next_start = self.value[..next_end].rfind('\n').map_or(0, |i| i + 1);
            (next_start, next_end)
        };
        let wanted = physical_width(&self.value[start..self.cursor]);
        self.cursor = next_start + boundary_at_column(&self.value[next_start..next_end], wanted);
    }
}

fn tab_width(col: usize) -> usize {
    4 - col % 4
}

fn physical_width(text: &str) -> usize {
    text.chars().fold(0, |col, c| {
        col + if c == '\t' {
            tab_width(col)
        } else {
            c.width().unwrap_or(0)
        }
    })
}

fn boundary_at_column(text: &str, wanted: usize) -> usize {
    let mut col = 0;
    for (index, c) in text.char_indices() {
        let cells = if c == '\t' {
            tab_width(col)
        } else {
            c.width().unwrap_or(0)
        };
        if col + cells > wanted {
            return index;
        }
        col += cells;
    }
    text.len()
}

fn caret_cell(col: usize, row: usize, width: usize) -> (usize, usize) {
    if col == width {
        (0, row + 1)
    } else {
        (col, row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn composer(text: &str) -> Composer {
        let mut input = Composer::default();
        input.insert(text).unwrap();
        input
    }

    fn key(input: &mut Composer, code: KeyCode) {
        input.key(KeyEvent::new(code, KeyModifiers::NONE)).unwrap();
    }

    #[test]
    fn paste_accepts_more_than_four_thousand_characters_and_rejects_overflow_atomically() {
        let mut input = composer(&"á".repeat(20_000));
        input.cursor = 2;
        let before = input.value.clone();
        assert!(input.insert(&"界".repeat(10_000)).is_err());
        assert_eq!(input.value, before);
        assert_eq!(input.cursor, 2);
        input.insert(&"x".repeat(MAX_BYTES - before.len())).unwrap();
        assert_eq!(input.value.len(), MAX_BYTES);
        assert!(input.insert("!").is_err());
    }

    #[test]
    fn paste_preserves_multiline_and_tabs_without_terminal_controls() {
        let input = composer("a\r\nb\rc\t\u{1b}[31m\0\u{7}!");
        assert_eq!(input.value, "a\nb\nc\t[31m!");
        assert_eq!(input.cursor, input.value.len());
    }

    #[test]
    fn edits_utf8_at_the_insertion_point_and_does_not_insert_shortcut_characters() {
        let mut input = composer("a界🙂z");
        key(&mut input, KeyCode::Left);
        key(&mut input, KeyCode::Backspace);
        assert_eq!(input.value, "a界z");
        key(&mut input, KeyCode::Left);
        key(&mut input, KeyCode::Delete);
        key(&mut input, KeyCode::Char('é'));
        assert_eq!(input.value, "aéz");
        assert_eq!(input.cursor, "aé".len());
        input
            .key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(input.value, "aéz");
        input
            .key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!((input.value.as_str(), input.cursor), ("", 0));
    }

    #[test]
    fn home_end_and_vertical_navigation_use_physical_lines_and_display_columns() {
        let mut input = composer("ab界e\n\t界z\nx\nfi");
        input.cursor = "ab界".len();
        key(&mut input, KeyCode::Down);
        assert_eq!(input.cursor, "ab界e\n\t".len());
        key(&mut input, KeyCode::Up);
        assert_eq!(input.cursor, "ab界".len());
        key(&mut input, KeyCode::Home);
        assert_eq!(input.cursor, 0);
        key(&mut input, KeyCode::Up);
        assert_eq!(input.cursor, 0);
        key(&mut input, KeyCode::Down);
        key(&mut input, KeyCode::End);
        assert_eq!(input.cursor, "ab界e\n\t界z".len());
        key(&mut input, KeyCode::Down);
        assert_eq!(input.cursor, "ab界e\n\t界z\nx".len());
        key(&mut input, KeyCode::Down);
        assert_eq!(input.cursor, input.value.len() - 1);
        key(&mut input, KeyCode::Down);
        assert_eq!(input.cursor, input.value.len() - 1);
    }

    #[test]
    fn vertical_navigation_keeps_combining_marks_with_their_base() {
        let mut input = composer("e\u{301}x\na!");
        input.cursor = "e\u{301}x\na".len();
        key(&mut input, KeyCode::Up);
        assert_eq!(input.cursor, "e\u{301}".len());
    }

    #[test]
    fn wrapping_tracks_wide_glyphs_combining_marks_and_newline_carets() {
        let mut input = composer("a界e\u{301}\nxy");
        assert_eq!(
            input.lines(3),
            (vec!["a界".into(), "e\u{301}".into(), "xy".into()], (2, 2))
        );
        input.cursor = "a界".len();
        assert_eq!(input.lines(3).1, (0, 1));
        input.cursor = "a界e\u{301}".len();
        assert_eq!(input.lines(3).1, (1, 1));
        input.cursor += 1;
        assert_eq!(input.lines(3).1, (0, 2));
        let input = composer("ab界");
        assert_eq!(input.lines(3), (vec!["ab".into(), "界".into()], (2, 1)));
        assert_eq!(
            composer("e\u{301}").lines(1),
            (vec!["e\u{301}".into(), "".into()], (0, 1))
        );
    }

    #[test]
    fn tabs_expand_to_four_cell_stops_and_wrap_without_losing_caret() {
        let mut input = composer("a\tb\t!");
        assert_eq!(
            input.lines(5),
            (vec!["a   b".into(), "   !".into()], (4, 1))
        );
        input.cursor = 2;
        assert_eq!(input.lines(5).1, (4, 0));
        assert_eq!(
            composer("\t").lines(3),
            (vec!["   ".into(), " ".into()], (1, 1))
        );
    }

    #[test]
    fn zero_and_one_column_views_stay_within_their_viewport() {
        let input = composer("界\n🙂");
        assert_eq!(input.lines(0), (vec![String::new()], (0, 0)));
        let (lines, caret) = input.lines(1);
        assert_eq!(caret, (0, 2));
        assert!(lines.iter().all(|line| line.width() <= 1));
    }

    #[test]
    fn clicking_wide_and_combining_glyphs_preserves_utf8_and_cluster_boundaries() {
        let mut input = composer("a界e\u{301}z\nfim");
        input.place_cursor(2, 0, 4);
        assert_eq!(input.cursor, "a".len());
        assert_eq!(input.lines(4).1, (1, 0));
        input.place_cursor(3, 0, 4);
        assert_eq!(input.cursor, "a界".len());
        input.place_cursor(0, 1, 4);
        assert_eq!(input.cursor, "a界e\u{301}".len());
        assert_eq!(input.lines(4).1, (0, 1));
        input.insert("!").unwrap();
        assert_eq!(input.value, "a界e\u{301}!z\nfim");

        let mut combining = composer("e\u{301}x");
        combining.place_cursor(1, 0, 8);
        assert_eq!(combining.cursor, "e\u{301}".len());
        combining.insert("!").unwrap();
        assert_eq!(combining.value, "e\u{301}!x");
    }

    #[test]
    fn clicking_line_end_handles_wrapping_unused_cells_and_explicit_newlines() {
        let mut input = composer("abcdef");
        input.place_cursor(99, 0, 3);
        assert_eq!(input.cursor, 3);
        assert_eq!(input.lines(3).1, (0, 1));
        input.place_cursor(0, 2, 3);
        assert_eq!(input.cursor, 6);
        assert_eq!(input.lines(3).1, (0, 2));

        let mut wide = composer("ab界");
        wide.place_cursor(2, 0, 3);
        assert_eq!(wide.cursor, 2);
        assert_eq!(wide.lines(3).1, (0, 1));

        let mut multiline = composer("abc\nx\n\nlast");
        multiline.place_cursor(50, 0, 8);
        assert_eq!(multiline.cursor, 3);
        multiline.place_cursor(9, 2, 8);
        assert_eq!(multiline.cursor, "abc\nx\n".len());
        multiline.place_cursor(0, usize::MAX, 8);
        assert_eq!(multiline.cursor, multiline.value.len());
    }

    #[test]
    fn clicking_expanded_tabs_selects_existing_insertion_points_without_changing_text() {
        let mut input = composer("a\tb\t!");
        let original = input.value.clone();
        input.place_cursor(2, 0, 5);
        assert_eq!(input.cursor, 1);
        input.place_cursor(3, 0, 5);
        assert_eq!(input.cursor, 2);
        input.place_cursor(3, 1, 5);
        assert_eq!(input.cursor, 4);
        assert_eq!(input.lines(5).1, (3, 1));
        assert_eq!(input.value, original);

        let mut wrapped = composer("\t");
        wrapped.place_cursor(0, 1, 3);
        assert_eq!(wrapped.cursor, 1);
        assert_eq!(wrapped.lines(3).1, (1, 1));
    }

    #[test]
    fn cursor_placement_handles_zero_width_and_round_trips_visible_boundaries() {
        let mut input = composer("á界e\u{301}\nabc\t🙂");
        input.place_cursor(40, 50, 0);
        assert_eq!(input.cursor, 0);
        for width in [1, 3, 6, 12] {
            for index in input
                .value
                .char_indices()
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
            {
                input.cursor = index;
                let (column, row) = input.lines(width).1;
                input.place_cursor(column, row, width);
                assert!(input.value.is_char_boundary(input.cursor));
                assert_eq!(input.lines(width).1, (column, row));
            }
        }
    }
}
