use color_eyre::eyre::Result;
use ratatui::{
    Frame,
    prelude::*,
    widgets::{Block, Borders, Paragraph},
};

use super::{Action, Component};

pub enum Role {
    User,
    #[allow(dead_code)]
    Bot,
}

pub enum Content {
    Plain(String),
    #[allow(dead_code)]
    Noop,
}

pub struct Message {
    pub role: Role,
    pub content: Content,
}

impl Message {
    pub fn height(&self) -> usize {
        let content = if let Content::Plain(ref text) = self.content {
            text.split("\n").count()
        } else {
            1
        };
        content + 1 // with border
    }
}

impl Component for Message {
    #[allow(unused_variables)]
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        Ok(None)
    }
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::*;

        let block = Block::new().borders(Borders::BOTTOM);
        frame.render_widget(&block, area);
        let area = block.inner(area);

        let [area_role, area_content] = Layout::horizontal([Length(8), Min(1)]).areas(area);
        if let Content::Plain(ref content) = self.content {
            let line = Line::from(vec![content.into()]);
            let paragraph = Paragraph::new(line);
            frame.render_widget(paragraph, area_content);
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
