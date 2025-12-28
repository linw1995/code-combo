use coco_macro::{ComponentExt, ContentComponentExt};
use code_highlight::{Event, Lang, highlight};
use ratatui::{
    Frame,
    prelude::Rect,
    style::Style,
    text::{Line, Span, Text},
    widgets::Wrap,
};
use serde::{Deserialize, Serialize};
use snafu::ResultExt;
use tracing::trace;

use super::{Component, Content, ContentComponent};
use crate::{
    components::Persistable,
    error::Result,
    global,
    session::{self, Session},
    widgets::Paragraph,
};

#[derive(Debug, Serialize, Deserialize)]
struct State {
    source: String,
    lang: Lang,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "code_highlight")]
pub struct CodeHighlight<'a> {
    state: State,
    widget: Paragraph<'a>,
    theme_version: usize,
}

impl<'a> CodeHighlight<'a> {
    pub fn try_new(source: &str, lang: Lang) -> Result<Self> {
        let widget = Self::build_widget(source, lang)?;
        Ok(Self {
            state: State {
                source: source.to_string(),
                lang,
            },
            widget,
            theme_version: global::theme_version(),
        })
    }

    fn build_widget(source: &str, lang: Lang) -> Result<Paragraph<'static>> {
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
        let widget = Paragraph::new_wrap(
            Text::from(lines.into_iter().map(Line::from).collect::<Vec<_>>()),
            Wrap { trim: false },
        );
        Ok(widget)
    }
}

impl Persistable for CodeHighlight<'static> {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: State = session::load(session)?;
        Self::try_new(&state.source, state.lang)
    }
}

impl Component for CodeHighlight<'static> {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        if self.theme_version != global::theme_version() {
            self.widget = Self::build_widget(&self.state.source, self.state.lang)?;
            self.theme_version = global::theme_version();
        }
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
