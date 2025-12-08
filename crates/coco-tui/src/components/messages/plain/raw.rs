use coco_macro::{ComponentExt, ContentComponentExt};
use ratatui::{Frame, prelude::Rect, widgets::Wrap};

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
        Self {
            text: text.clone(),
            widget: Paragraph::new_wrap(text, Wrap { trim: false }),
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
