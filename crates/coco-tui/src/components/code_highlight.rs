use code_highlight::{Event, Lang, highlight};
use color_eyre::Result;
use ratatui::{
    Frame,
    prelude::Rect,
    style::Style,
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
};
use tracing::trace;

use super::{Component, Content, ContentComponent};

pub struct CodeHighlight<'a> {
    widget: Paragraph<'a>,
}

impl<'a> CodeHighlight<'a> {
    pub fn try_new(source: &str, lang: Lang, colorscheme: &str) -> Result<Self> {
        use Event::*;

        let colorscheme = colorschemes::use_builtin_colorscheme(colorscheme)
            .expect("failed to use built-in colorscheme");

        let names = colorscheme.keys().map(|x| x.as_str()).collect::<Vec<_>>();
        trace!(?names, "highlighting with color scheme");
        let events = highlight(&lang, &names, source)?;

        let mut line = vec![];
        let mut lines = vec![];
        let mut styles: Vec<Style> = vec![];
        let default_style = Style::default();
        for event in events {
            match event {
                Start(kind) => styles.push(
                    colorscheme
                        .get(kind)
                        .map(Style::from)
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

mod colorschemes {
    use ratatui::style::{Color, Modifier, Style};
    use serde::{Deserialize, Serialize};

    use lazy_static::lazy_static;
    use std::collections::HashMap;
    use tracing::warn;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum ColorSchemeStyle {
        Advance(ColorSchemeStyleAdvance),
        Fg(Color),
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    pub struct ColorSchemeStyleAdvance {
        fg: Option<Color>,
        bg: Option<Color>,
        underline_color: Option<Color>,
        add_modifier: Option<String>,
        sub_modifier: Option<String>,
    }

    impl From<&ColorSchemeStyle> for Style {
        fn from(value: &ColorSchemeStyle) -> Self {
            let ColorSchemeStyleAdvance {
                fg,
                bg,
                underline_color,
                add_modifier,
                sub_modifier,
            } = match value {
                ColorSchemeStyle::Fg(fg) => ColorSchemeStyleAdvance {
                    fg: Some(fg.to_owned()),
                    ..Default::default()
                },
                ColorSchemeStyle::Advance(value) => value.to_owned(),
            };

            let add_modifier = add_modifier
                .map(|name| match bitflags::parser::from_str(&name) {
                    Err(err) => {
                        warn!(?name, ?err, "invalid add_modifier of style");
                        Modifier::empty()
                    }
                    Ok(v) => v,
                })
                .unwrap_or_default();

            let sub_modifier = sub_modifier
                .map(|name| match bitflags::parser::from_str(&name) {
                    Err(err) => {
                        warn!(?name, ?err, "invalid sub_modifier of style");
                        Modifier::empty()
                    }
                    Ok(v) => v,
                })
                .unwrap_or_default();

            Self {
                fg,
                bg,
                underline_color,
                add_modifier,
                sub_modifier,
            }
        }
    }

    pub type ColorScheme = HashMap<String, ColorSchemeStyle>;

    lazy_static! {
        pub static ref CATPPUCCIN_MOCHA: ColorScheme =
            serde_json::from_str(include_str!("../../colorscheme/catppuccin_mocha.json"))
                .expect("failed to load catppuccin_mocha colorscheme");
    }

    pub fn use_builtin_colorscheme(name: &str) -> Option<ColorScheme> {
        match name {
            "catppuccin_mocha" => Some(CATPPUCCIN_MOCHA.clone()),
            _ => None,
        }
    }
}
