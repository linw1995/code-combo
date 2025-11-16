use code_highlight::{Event, Lang, highlight};
use color_eyre::Result;
use ratatui::{
    Frame,
    prelude::Rect,
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
};
use tracing::warn;

use super::{Component, Content, ContentComponent};

pub struct CodeHighlight<'a> {
    widget: Paragraph<'a>,
}

impl<'a> CodeHighlight<'a> {
    pub fn try_new(source: &str, lang: Lang) -> Result<Self> {
        use Event::*;

        let events = highlight(&lang, source)?;

        let mut line = vec![];
        let mut lines = vec![];
        let mut styles: Vec<Style> = vec![];
        let default_style = Style::default();
        for event in events {
            match event {
                Start(kind) => styles.push(match kind {
                    "string" => Style::default().fg(Color::Green),
                    "function" => Style::default().fg(Color::Cyan),
                    "operator" => Style::default().fg(Color::Yellow),
                    _ => {
                        warn!(?kind, "unknown highlight kind");
                        Style::default().fg(Color::DarkGray)
                    }
                }),
                Source(src) => {
                    let style = styles.last().unwrap_or(&default_style);
                    if src.contains("\n") {
                        let parts = src.split("\n");
                        for part in parts {
                            line.push(Span::styled(part.to_owned(), *style));
                            lines.push(line);
                            line = vec![];
                        }
                        line = lines.pop().unwrap();
                    } else {
                        line.push(Span::styled(src.to_owned(), *style));
                    }
                }
                End => {
                    styles.pop();
                }
            }
        }
        lines.push(line);
        Ok(Self {
            widget: Paragraph::new(Text::from(
                lines.into_iter().map(Line::from).collect::<Vec<_>>(),
            ))
            .wrap(Wrap { trim: false }),
        })
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
