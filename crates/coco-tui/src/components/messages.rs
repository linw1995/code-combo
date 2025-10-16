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
pub use combo::Combo;

pub enum Role {
    User,
    #[allow(dead_code)]
    Bot,
}

pub enum Content {
    Plain(String),
    Combo(Combo),
}

pub struct Message {
    pub role: Role,
    pub content: Content,
}

impl Message {
    pub fn height(&self) -> usize {
        let content = match self.content {
            Content::Plain(ref text) => text.split("\n").count(),
            Content::Combo(_) => 3,
            #[allow(unreachable_patterns)]
            _ => 1,
        };
        content + 1 // with border
    }
}

impl Component for Message {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        match &mut self.content {
            Content::Combo(combo) => Box::new(vec![combo as &mut dyn Component].into_iter()),
            _ => Box::new(std::iter::empty()),
        }
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
        match &mut self.content {
            Content::Plain(text) => {
                let line = Line::from(vec![text.to_owned().into()]);
                let paragraph = Paragraph::new(line);
                frame.render_widget(paragraph, area_content);
            }
            Content::Combo(combo) => {
                combo.draw(frame, area_content)?;
            }
        }

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
