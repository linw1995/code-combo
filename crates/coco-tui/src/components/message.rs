use std::any::Any;

use coco_macro::ComponentExt;
use crossterm::event::KeyEvent;
use ratatui::{
    Frame,
    prelude::*,
    widgets::{Block, Paragraph},
};
use serde::{Deserialize, Serialize};

use super::Component;
use crate::{
    components::{Persistable, Tool},
    error::*,
    global::{self},
    session::{self, Session},
};

#[derive(Serialize, Deserialize)]
pub enum Role {
    User,
    Bot,
}

/// The content of `Message`;
pub trait Content {
    /// the height of content rect.
    fn height(&self, width: u16) -> usize;

    /// Check if the content is actionable.
    fn is_actionable(&self) -> bool {
        false
    }

    /// Display shortcuts description on the block bottom of the chat window.
    fn block_with_shortcuts_desc<'a>(&self, block: Block<'a>) -> Block<'a> {
        block
    }
}

pub trait ContentComponent: Component + Content {}

impl<T: ContentComponent> From<T> for Box<dyn ContentComponent> {
    fn from(value: T) -> Self {
        Box::new(value)
    }
}

impl dyn ContentComponent {
    /// Allow downcasting the trait object to its concrete type at runtime
    pub fn as_any(&self) -> &dyn Any {
        self
    }

    /// Allow downcasting the trait object to its concrete type at runtime
    pub fn as_mut_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// A message component in the Chat component
#[derive(ComponentExt)]
#[component(type_id = "message")]
pub struct Message {
    state: Inner,
    content: Box<dyn ContentComponent>,
}

#[derive(Serialize, Deserialize)]
struct Inner {
    pub role: Role,
    pub content_type_id: String,
}

impl Message {
    pub fn bot(content: Box<dyn ContentComponent>) -> Self {
        Self {
            state: Inner {
                role: Role::Bot,
                content_type_id: content.id().to_string(),
            },
            content,
        }
    }

    pub fn user(content: Box<dyn ContentComponent>) -> Self {
        Self {
            state: Inner {
                role: Role::User,
                content_type_id: content.id().to_string(),
            },
            content,
        }
    }

    pub fn is_same_tool_id(&self, id: &str) -> bool {
        self.content
            .as_any()
            .downcast_ref::<Tool>()
            .map(|tool| tool.tool_use_id() == id)
            .unwrap_or_default()
    }
}

// Delegate Content trait to its inner content.
impl Content for Message {
    fn height(&self, width: u16) -> usize {
        let role_width = match self.state.role {
            Role::User => 7,
            Role::Bot => 6,
        };
        self.content.height(width.saturating_sub(role_width))
    }

    fn is_actionable(&self) -> bool {
        self.content.is_actionable()
    }

    fn block_with_shortcuts_desc<'a>(&self, block: Block<'a>) -> Block<'a> {
        self.content.block_with_shortcuts_desc(block)
    }
}

impl Persistable for Message {
    fn save(&self) -> Session {
        session::save_related(&self.state, self.content.save())
    }

    fn load(session: Session) -> Result<Self> {
        let (state, child): (Inner, Session) = session::load_related(session)?;
        let content = session::load_content_component(&state.content_type_id, child)?;
        Ok(Self { state, content })
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

        let theme = global::theme();
        let paragraph = Paragraph::new(Line::from(match self.state.role {
            Role::User => Span::styled(" User: ", theme.ui.user_role),
            Role::Bot => Span::styled(" Bot: ", theme.ui.bot_role),
        }));
        frame.render_widget(paragraph, area_role);

        Ok(())
    }
}

impl ContentComponent for Message {}
