use ratatui::{
    prelude::{Buffer, Rect},
    text::{Line, Span, Text},
    widgets::{Widget, WidgetRef, Wrap},
};

use crate::global;

/// Customized Paragraph to safely render characters.
///
/// The standard ratatui Paragraph widget has issues with tab character (`\t`) rendering.
/// When scrolling, the area occupied by a tab character may retain previous render content,
/// causing visual artifacts. This wrapper ensures safe rendering by handling such edge cases.
pub struct Paragraph<'a> {
    inner: ratatui::widgets::Paragraph<'a>,
}

impl<'a> Paragraph<'a> {
    pub fn new<T>(text: T) -> Self
    where
        T: Into<Text<'a>>,
    {
        let mut text: Text = text.into();
        for line in &mut text.lines {
            safe_line(line);
        }
        Self {
            inner: ratatui::widgets::Paragraph::new(text),
        }
    }

    pub fn new_wrap<T>(text: T, wrap: Wrap) -> Self
    where
        T: Into<Text<'a>>,
    {
        let mut text: Text = text.into();
        for line in &mut text.lines {
            safe_line(line);
        }
        Self {
            inner: ratatui::widgets::Paragraph::new(text).wrap(wrap),
        }
    }

    pub fn line_count(&self, width: u16) -> usize {
        self.inner.line_count(width)
    }
}

impl WidgetRef for Paragraph<'static> {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        self.inner.render_ref(area, buf);
    }
}

impl Widget for Paragraph<'static> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.inner.render(area, buf);
    }
}

const TAB_STOP: usize = 4;

/// Replace specific characters like `\t` to avoid rendering issues or provide stylized looking in ratatui.
///
/// This function processes a line of text and replaces problematic characters:
/// - Tab characters (`\t`) are replaced with a visible tab indicator ("▸" followed by spaces)
/// - Space characters at tab stop boundaries are replaced with "│" (a visible space indicator)
///
/// These replacements prevent visual artifacts that can occur when ratatui renders
/// these characters, particularly during scrolling operations.
///
/// # Tab Stop Alignment
///
/// The function maintains proper tab stop alignment with a tab stop size of 4 columns.
/// The tab indicator ("▸") is positioned based on the current column position:
/// - At column 0 (mod 4): "▸   " (4 columns)
/// - At column 1 (mod 4): "▸  " (3 columns)
/// - At column 2 (mod 4): "▸ " (2 columns)
/// - At column 3 (mod 4): "▸" (1 column)
///
/// This ensures that the next character after a tab starts at the next tab stop boundary.
///
/// # Space Handling
///
/// Space characters at tab stop boundaries (every 4 columns) are replaced with "│"
/// when they are at the start of a line or adjacent to another space character.
/// This makes indentation levels visible while maintaining proper column alignment.
/// Single isolated spaces at tab boundaries that are not part of an indentation
/// sequence are preserved as-is.
///
/// # Styling
///
/// The visual markers ("▸" for tabs and "│" for spaces) inherit the `tab_spaces`
/// style from the current theme, allowing consistent theming across the application.
/// Regular text preserves its original style.
fn safe_line(line: &mut Line) {
    let theme = global::theme();
    let tab_spaces_style = theme.ui.tab_spaces;

    let mut new_spans = Vec::with_capacity(line.spans.len());
    let mut col = 0;
    for span in &line.spans {
        let content = &span.content;
        let chars = content.chars().collect::<Vec<_>>();
        let mut i = 0;
        let mut width = 0;

        while i < chars.len() {
            if chars[i] == '\t' {
                let (sep, sep_width) = match (col + width) % TAB_STOP {
                    0 => ("▸   ", 4),
                    1 => ("▸  ", 3),
                    2 => ("▸ ", 2),
                    3 => ("▸", 1),
                    _ => unreachable!(),
                };
                new_spans.push(Span::styled(sep, tab_spaces_style));
                i += 1;
                width += sep_width;
            } else if chars[i] == ' '
                && (col + width) % TAB_STOP == 0
                && (i == 0 || chars[i - 1] == ' ' || (i + 1 < chars.len() && chars[i + 1] == ' '))
            {
                new_spans.push(Span::styled("│", tab_spaces_style));
                i += 1;
                width += 1;
            } else {
                // Collect regular characters
                let start = i;
                while i < chars.len()
                    && chars[i] != '\t'
                    && !(chars[i] == ' '
                        && (col + width) % TAB_STOP == 0
                        && (i == 0
                            || chars[i - 1] == ' '
                            || (i + 1 < chars.len() && chars[i + 1] == ' ')))
                {
                    i += 1;
                    width += 1;
                }

                let regular_content = chars[start..i].iter().collect::<String>();
                if !regular_content.is_empty() {
                    new_spans.push(Span::styled(regular_content, span.style));
                }
            }
        }
        col += width
    }
    line.spans = new_spans;
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn simple() {
        let widget = Paragraph::new("\t\tHello\n\tWorld\n    Hello\n    World");

        let mut terminal = Terminal::new(TestBackend::new(17, 4)).unwrap();
        terminal
            .draw(|frame| widget.render(frame.area(), frame.buffer_mut()))
            .unwrap();

        let mut expected = Buffer::with_lines(vec![
            "▸   ▸   Hello    ",
            "▸   World        ",
            "│   Hello        ",
            "│   World        ",
        ]);

        // Apply the tab_spaces style to the special symbols
        let tab_spaces_style = global::theme().ui.tab_spaces;

        // Style the first tab symbol on line 0 (positions 0-3)
        expected.set_style(Rect::new(0, 0, 4, 1), tab_spaces_style);
        // Style the second tab symbol on line 0 (positions 4-7)
        expected.set_style(Rect::new(4, 0, 4, 1), tab_spaces_style);

        // Style the tab symbol on line 1
        expected.set_style(Rect::new(0, 1, 4, 1), tab_spaces_style);

        // Style the space symbol on line 2
        expected.set_style(Rect::new(0, 2, 1, 1), tab_spaces_style);

        // Style the space symbol on line 3
        expected.set_style(Rect::new(0, 3, 1, 1), tab_spaces_style);

        assert_eq!(terminal.backend().buffer(), &expected);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tab_stop() {
        let widget = Paragraph::new("ab\t\tHello\n\tWorld\n     Hello\n+      World");

        let mut terminal = Terminal::new(TestBackend::new(17, 4)).unwrap();
        terminal
            .draw(|frame| widget.render(frame.area(), frame.buffer_mut()))
            .unwrap();

        let mut expected = Buffer::with_lines(vec![
            "ab▸ ▸   Hello    ",
            "▸   World        ",
            "│   │Hello       ",
            "+   │  World     ",
        ]);

        // Apply the tab_spaces style to the special symbols
        let tab_spaces_style = global::theme().ui.tab_spaces;

        // Style the first tab symbol on line 0
        expected.set_style(Rect::new(2, 0, 6, 1), tab_spaces_style);

        // Style the tab symbol on line 1
        expected.set_style(Rect::new(0, 1, 4, 1), tab_spaces_style);

        // Style the space symbol on line 2
        expected.set_style(Rect::new(0, 2, 1, 1), tab_spaces_style);
        expected.set_style(Rect::new(4, 2, 1, 1), tab_spaces_style);

        // Style the space symbol on line 3
        expected.set_style(Rect::new(4, 3, 1, 1), tab_spaces_style);

        assert_eq!(terminal.backend().buffer(), &expected);
    }
}
