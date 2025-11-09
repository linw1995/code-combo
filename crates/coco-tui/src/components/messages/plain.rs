use ratatui::{
    Frame,
    prelude::Rect,
    widgets::{Paragraph, Wrap},
};

use crate::components::ContentComponent;

use super::{Component, Content};

pub struct Plain<'a> {
    widget: Paragraph<'a>,
}

impl<'a> Plain<'a> {
    pub fn new(text: String) -> Self {
        Self {
            widget: Paragraph::new(text).wrap(Wrap { trim: false }),
        }
    }
}

impl<'a> Component for Plain<'a> {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> color_eyre::eyre::Result<()> {
        frame.render_widget(&self.widget, area);
        Ok(())
    }
}

impl<'a> Content for Plain<'a> {
    fn height(&self, width: u16) -> usize {
        self.widget.line_count(width)
    }
}

impl ContentComponent for Plain<'static> {}
