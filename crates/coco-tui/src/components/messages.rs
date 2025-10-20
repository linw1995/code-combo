use color_eyre::Result;
use ratatui::{
    Frame,
    prelude::*,
    widgets::{Block, Borders, Paragraph},
};
use tracing::debug;

use super::Component;
use crate::actions::Action;

mod combo;
mod plain;
pub use combo::Combo;
pub use plain::Plain;

pub enum Role {
    User,
    #[allow(dead_code)]
    Bot,
}

pub trait Content {
    fn height(&self) -> usize;
}

pub trait ContentComponent: Component + Content {}

pub struct Message {
    pub role: Role,
    pub content: Box<dyn ContentComponent>,
}

impl Message {
    pub fn height(&self) -> usize {
        self.content.height() + 1 // with border
    }
}

impl Component for Message {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(vec![self.content.as_mut() as &mut dyn Component].into_iter())
    }

    fn update(&mut self, action: &Action) {
        debug!(?action, "updating");
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::*;

        let block = Block::new().borders(Borders::BOTTOM);
        frame.render_widget(&block, area);
        let area = block.inner(area);

        let [area_role, area_content] = Layout::horizontal([Length(8), Min(1)]).areas(area);
        self.content.draw(frame, area_content)?;

        let paragraph = Paragraph::new(Line::from(
            match self.role {
                Role::User => " User: ".green(),
                Role::Bot => " Bot: ".blue(),
            }
            .bold(),
        ));
        frame.render_widget(paragraph, area_role);

        Ok(())
    }
}
