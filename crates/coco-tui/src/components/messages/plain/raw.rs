use color_eyre::Result;
use ratatui::{
    Frame,
    prelude::Rect,
    widgets::{Paragraph, Wrap},
};

use crate::components::{Component, Content, ContentComponent};

pub struct RawTextViewer<'a> {
    widget: Paragraph<'a>,
}

impl<'a> RawTextViewer<'a> {
    pub fn new(text: String) -> Self {
        Self {
            widget: Paragraph::new(text).wrap(Wrap { trim: false }),
        }
    }
}

impl<'a> Component for RawTextViewer<'a> {
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
