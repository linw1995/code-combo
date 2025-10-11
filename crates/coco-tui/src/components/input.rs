use color_eyre::eyre::Result;
use ratatui::{Frame, prelude::*};

use super::Component;

#[derive(Default)]
#[allow(dead_code)]
pub struct Input<'a> {
    pub textarea: tui_textarea::TextArea<'a>,
}

impl Component for Input<'_> {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        frame.render_widget(&self.textarea, area);
        Ok(())
    }
}
