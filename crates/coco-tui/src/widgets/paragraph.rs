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
    text: Text<'a>,
    wrap: Option<Wrap>,
}

impl<'a> Paragraph<'a> {
    pub fn new<T>(text: T) -> Self
    where
        T: Into<Text<'a>>,
    {
        Self {
            text: text.into(),
            wrap: None,
        }
    }

    pub fn new_wrap<T>(text: T, wrap: Wrap) -> Self
    where
        T: Into<Text<'a>>,
    {
        Self {
            text: text.into(),
            wrap: Some(wrap),
        }
    }

    pub fn line_count(&self, width: u16) -> usize {
        self.build_widget().line_count(width)
    }

    fn build_widget(&self) -> ratatui::widgets::Paragraph<'a> {
        let mut text = self.text.clone();
        for line in &mut text.lines {
            safe_line(line);
        }
        let mut widget = ratatui::widgets::Paragraph::new(text);
        if let Some(wrap) = self.wrap {
            widget = widget.wrap(wrap);
        }
        widget
    }
}

impl WidgetRef for Paragraph<'static> {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        self.build_widget().render_ref(area, buf);
    }
}

impl Widget for Paragraph<'static> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.build_widget().render(area, buf);
    }
}

const TAB_STOP: usize = 4;

/// Replace tab character (`\t`) to avoid rendering issues or provide stylized looking in ratatui.
///
/// This function processes a line of text and replaces problematic characters:
/// - Tab characters (`\t`) are replaced with a visible tab indicator ("▸" followed by spaces)
///
/// This replacement prevents visual artifacts that can occur when ratatui renders
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
/// # Styling
///
/// The visual markers ("▸" for tabs) inherit the `tab_spaces`
/// style from the current theme, allowing consistent theming across the application.
/// Regular text is patched with the base `text` style so it follows theme changes
/// unless an explicit style is already set.
fn safe_line(line: &mut Line) {
    let theme = global::theme();
    let tab_spaces_style = theme.ui.tab_spaces;
    let base_style = theme.ui.text;
    let tab_style = base_style.patch(tab_spaces_style);

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
                new_spans.push(Span::styled(sep, tab_style));
                i += 1;
                width += sep_width;
            } else {
                // Collect regular characters
                let start = i;
                while i < chars.len() && chars[i] != '\t' {
                    i += 1;
                    width += 1;
                }

                let regular_content = chars[start..i].iter().collect::<String>();
                if !regular_content.is_empty() {
                    let style = base_style.patch(span.style);
                    new_spans.push(Span::styled(regular_content, style));
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
        let widget = Paragraph::new("\t\tHello\n\tWorld");

        let mut terminal = Terminal::new(TestBackend::new(17, 2)).unwrap();
        terminal
            .draw(|frame| widget.render(frame.area(), frame.buffer_mut()))
            .unwrap();

        let mut expected = Buffer::with_lines(vec!["▸   ▸   Hello    ", "▸   World        "]);

        // Apply the tab_spaces style to the special symbols
        let tab_spaces_style = global::theme().ui.tab_spaces;
        let text_style = global::theme().ui.text;

        // Style the first tab symbol on line 0 (positions 0-3)
        expected.set_style(Rect::new(0, 0, 4, 1), tab_spaces_style);
        // Style the second tab symbol on line 0 (positions 4-7)
        expected.set_style(Rect::new(4, 0, 4, 1), tab_spaces_style);

        // Style the tab symbol on line 1
        expected.set_style(Rect::new(0, 1, 4, 1), tab_spaces_style);
        expected.set_style(Rect::new(8, 0, 5, 1), text_style);
        expected.set_style(Rect::new(4, 1, 5, 1), text_style);

        assert_eq!(terminal.backend().buffer(), &expected);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tab_stop() {
        let widget = Paragraph::new("ab\t\tHello\n\tWorld");

        let mut terminal = Terminal::new(TestBackend::new(17, 2)).unwrap();
        terminal
            .draw(|frame| widget.render(frame.area(), frame.buffer_mut()))
            .unwrap();

        let mut expected = Buffer::with_lines(vec!["ab▸ ▸   Hello    ", "▸   World        "]);

        // Apply the tab_spaces style to the special symbols
        let tab_spaces_style = global::theme().ui.tab_spaces;
        let text_style = global::theme().ui.text;

        // Style the first tab symbol on line 0
        expected.set_style(Rect::new(2, 0, 6, 1), tab_spaces_style);
        expected.set_style(Rect::new(0, 0, 2, 1), text_style);
        expected.set_style(Rect::new(8, 0, 5, 1), text_style);

        // Style the tab symbol on line 1
        expected.set_style(Rect::new(0, 1, 4, 1), tab_spaces_style);
        expected.set_style(Rect::new(4, 1, 5, 1), text_style);

        assert_eq!(terminal.backend().buffer(), &expected);
    }
}
