use coco_macro::{ComponentExt, ContentComponentExt};
use ratatui::{
    Frame,
    prelude::Rect,
    style::Style,
    text::{Line, Span, Text},
    widgets::Wrap,
};

use crate::{
    components::{Component, Content, ContentComponent, Persistable},
    error::*,
    session::{self, Session},
    widgets::Paragraph,
};

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "raw_text_viewer")]
pub struct RawTextViewer<'a> {
    text: String,

    widget: Paragraph<'a>,
}

impl<'a> RawTextViewer<'a> {
    pub fn new(text: String) -> Self {
        Self::new_with_style(text, Style::default())
    }

    pub fn new_with_style(text: String, style: Style) -> Self {
        let text_string = text.clone();
        let lines = text_string
            .split('\n')
            .map(|line| Line::from(Span::styled(line.to_string(), style)))
            .collect::<Vec<_>>();
        let text_widget = Text::from(lines);
        Self {
            text: text_string,
            widget: Paragraph::new_wrap(text_widget, Wrap { trim: false }),
        }
    }
}

impl Persistable for RawTextViewer<'static> {
    fn save(&self) -> Session {
        session::save(&self.text)
    }

    fn load(session: Session) -> Result<Self> {
        Ok(Self::new(session::load(session)?))
    }
}

impl Component for RawTextViewer<'static> {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        frame.render_widget(&self.widget, area);
        Ok(())
    }
}

impl<'a> Content for RawTextViewer<'a> {
    fn height(&self, width: u16) -> usize {
        self.widget.line_count(width)
    }
}

impl ContentComponent for RawTextViewer<'static> {}
