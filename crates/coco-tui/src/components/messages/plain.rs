use ratatui::{Frame, prelude::Rect, widgets::Paragraph};

use crate::components::ContentComponent;

use super::{Component, Content};

pub struct Plain(String);

impl Plain {
    pub fn new(text: String) -> Self {
        Self(text)
    }
}

impl Component for Plain {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> color_eyre::eyre::Result<()> {
        let p = Paragraph::new(self.0.clone());
        frame.render_widget(&p, area);
        Ok(())
    }
}

impl Content for Plain {
    fn height(&self) -> usize {
        self.0.split("\n").count()
    }
}

impl ContentComponent for Plain {}
