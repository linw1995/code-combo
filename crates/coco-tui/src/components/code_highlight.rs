use code_highlight::{Event, Lang, highlight};
use ratatui::{
    Frame,
    prelude::Rect,
    style::Style,
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
};
use snafu::ResultExt;
use tracing::trace;

use super::{Component, Content, ContentComponent};
use crate::{error::*, global};

pub struct CodeHighlight<'a> {
    widget: Paragraph<'a>,
}

impl<'a> CodeHighlight<'a> {
    pub fn try_new(source: &str, lang: Lang) -> Result<Self> {
        use Event::*;

        let theme = global::theme();
        let names = theme
            .tree_sitter
            .keys()
            .map(|x| x.as_str())
            .collect::<Vec<_>>();
        trace!(?names, "highlighting with color scheme");
        let events = highlight(&lang, &names, source).whatever_context("failed to highlight")?;

        let mut line = vec![];
        let mut lines = vec![];
        let mut styles: Vec<Style> = vec![];
        let default_style = Style::default();
        for event in events {
            match event {
                Start(kind) => styles.push(
                    theme
                        .tree_sitter
                        .get(kind)
                        .cloned()
                        .unwrap_or(default_style),
                ),
                Source(src) => {
                    let style = styles.last().cloned().unwrap_or(default_style);
                    if src.contains("\n") {
                        let parts = src.split("\n");
                        for part in parts {
                            line.push(Span::styled(part.to_owned(), style));
                            lines.push(line);
                            line = vec![];
                        }
                        line = lines.pop().unwrap();
                    } else {
                        line.push(Span::styled(src.to_owned(), style));
                    }
                }
                End => {
                    styles.pop();
                }
            }
        }
        lines.push(line);
        let widget = Paragraph::new(Text::from(
            lines.into_iter().map(Line::from).collect::<Vec<_>>(),
        ))
        .wrap(Wrap { trim: false });
        Ok(Self { widget })
    }
}

impl<'a> Component for CodeHighlight<'a> {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        frame.render_widget(&self.widget, area);
        Ok(())
    }
}

impl<'a> Content for CodeHighlight<'a> {
    fn height(&self, width: u16) -> usize {
        self.widget.line_count(width)
    }
}

impl ContentComponent for CodeHighlight<'static> {}
