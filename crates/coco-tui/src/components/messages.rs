use std::any::Any;

use color_eyre::Result;
use crossterm::event::KeyEvent;
use ratatui::{
    Frame,
    prelude::*,
    widgets::{Block, Paragraph},
};

use super::Component;

mod combo;
mod plain;
mod tool;
pub use combo::Combo;
pub use plain::Plain;
pub use tool::Tool;

pub enum Role {
    User,
    Bot,
}

/// The content of `Message`;
pub trait Content {
    /// the height of content rect.
    fn height(&self) -> usize;

    /// Check if the content is actionable.
    fn is_actionable(&self) -> bool {
        false
    }

    /// Display shortcuts description on the block bottom of the chat window.
    fn block_bottom_with_shortcuts_desc<'a>(&self, block: Block<'a>) -> Block<'a> {
        block
    }
}

pub trait ContentComponent: Component + Content + Any {
    fn boxed(self) -> Box<dyn ContentComponent>
    where
        Self: Sized + 'static,
    {
        Box::new(self)
    }
}

impl dyn ContentComponent {
    /// Allow downcasting the trait object to its concrete type at runtime
    pub fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A message component in the Chat component
pub struct Message {
    pub role: Role,
    pub content: Box<dyn ContentComponent>,
}

impl Message {
    pub fn bot(content: Box<dyn ContentComponent>) -> Self {
        Self {
            role: Role::Bot,
            content,
        }
    }

    pub fn user(content: Box<dyn ContentComponent>) -> Self {
        Self {
            role: Role::User,
            content,
        }
    }
}

// Delegate Content trait to its inner content.
impl Content for Message {
    fn height(&self) -> usize {
        self.content.height()
    }

    fn is_actionable(&self) -> bool {
        self.content.is_actionable()
    }

    fn block_bottom_with_shortcuts_desc<'a>(&self, block: Block<'a>) -> Block<'a> {
        self.content.block_bottom_with_shortcuts_desc(block)
    }
}

impl Component for Message {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(vec![self.content.as_mut() as &mut dyn Component].into_iter())
    }

    fn handle_key_event(&mut self, event: &KeyEvent) {
        // Delegate the handle_key_event to its inner content.
        self.content.handle_key_event(event);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::*;

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

impl ContentComponent for Message {}

pub(super) fn shortcuts_desc<'a>(pairs: &[(&str, &str)]) -> Line<'a> {
    let descs: Vec<&str> = pairs.iter().map(|(desc, _)| desc.to_owned()).collect();
    let mut spans = vec![Span::raw(format!(" {} ", descs.join("/")))];
    let last_idx = pairs.len() - 1;
    for (idx, (_, key)) in pairs.iter().enumerate() {
        spans.push(Span::raw(format!("<{key}>")).blue().bold());
        if idx != last_idx {
            spans.push(Span::raw("/"));
        }
    }
    spans.push(" ".into());
    Line::from(spans)
}
