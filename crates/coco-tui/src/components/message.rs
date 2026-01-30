use std::{any::Any, ops::Range};

use coco_macro::ComponentExt;
use crossterm::event::KeyEvent;
use ratatui::{Frame, prelude::*, widgets::Paragraph};
use serde::{Deserialize, Serialize};

use super::Thinking;
use super::{Component, NavigationKey, NavigationResult, ShortcutHints};
use crate::{
    components::{Combo, Persistable, Tool},
    error::*,
    global::{self},
    session::{self, Session},
};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Bot,
    System,
}

/// The content of `Message`;
pub trait Content {
    /// the height of content rect.
    fn height(&self, width: u16) -> usize;

    /// Provide a focused range (relative to the content area) for auto scrolling.
    fn focus_range(&self, _width: u16) -> Option<Range<u16>> {
        None
    }

    /// Check if the content is actionable.
    fn is_actionable(&self) -> bool {
        false
    }

    /// Provide shortcuts hints for the current content.
    fn shortcut_hints(&self) -> ShortcutHints {
        ShortcutHints::default()
    }

    /// Provide a reminder line to append to the message title.
    fn reminder_line(&self) -> Option<Line<'static>> {
        None
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
    #[serde(default = "default_show_role_prefix")]
    pub show_role_prefix: bool,
}

fn default_show_role_prefix() -> bool {
    true
}

impl Message {
    pub fn bot(content: Box<dyn ContentComponent>) -> Self {
        Self {
            state: Inner {
                role: Role::Bot,
                content_type_id: content.id().to_string(),
                show_role_prefix: true,
            },
            content,
        }
    }

    pub fn user(content: Box<dyn ContentComponent>) -> Self {
        Self {
            state: Inner {
                role: Role::User,
                content_type_id: content.id().to_string(),
                show_role_prefix: true,
            },
            content,
        }
    }

    pub fn system(content: Box<dyn ContentComponent>) -> Self {
        Self {
            state: Inner {
                role: Role::System,
                content_type_id: content.id().to_string(),
                show_role_prefix: true,
            },
            content,
        }
    }

    pub fn with_role_prefix(mut self, show_role_prefix: bool) -> Self {
        self.state.show_role_prefix = show_role_prefix;
        self
    }

    pub fn set_show_role_prefix(&mut self, show_role_prefix: bool) {
        self.state.show_role_prefix = show_role_prefix;
    }

    pub fn role(&self) -> &Role {
        &self.state.role
    }

    pub fn is_bot(&self) -> bool {
        matches!(self.state.role, Role::Bot)
    }

    pub fn is_same_tool_id(&self, id: &str) -> bool {
        // Check if this is a Tool with matching id
        if let Some(tool) = self.content.as_any().downcast_ref::<Tool>() {
            return tool.tool_use_id() == id;
        }
        // Check if this is a Combo with matching id (for Method 2 permission handling)
        if let Some(combo) = self.content.as_any().downcast_ref::<Combo>() {
            return combo.matches_id(id);
        }
        false
    }

    pub fn is_waiting_permission(&self) -> bool {
        if let Some(tool) = self.content.as_any().downcast_ref::<Tool>() {
            return tool.is_pending_confirmation();
        }
        if let Some(combo) = self.content.as_any().downcast_ref::<Combo>() {
            return combo.is_pending_permission();
        }
        false
    }

    pub fn thinking_mut(&mut self) -> Option<&mut Thinking> {
        self.content.as_mut_any().downcast_mut::<Thinking>()
    }

    pub fn is_thinking(&self) -> bool {
        self.content.as_any().downcast_ref::<Thinking>().is_some()
    }

    pub fn is_hidden(&self) -> bool {
        if let Some(thinking) = self.content.as_any().downcast_ref::<Thinking>() {
            return thinking.is_collapsed();
        }
        false
    }

    pub fn content_as_any(&self) -> &dyn Any {
        self.content.as_any()
    }

    pub fn content_as_mut_any(&mut self) -> &mut dyn Any {
        self.content.as_mut_any()
    }

    pub fn height_compact(&self, width: u16, compact: bool) -> usize {
        if let Some(thinking) = self.content.as_any().downcast_ref::<Thinking>()
            && thinking.is_collapsed()
        {
            return 0;
        }
        let role_width = if compact {
            2
        } else {
            match self.state.role {
                Role::User => 8,
                Role::Bot => 8,
                Role::System => 2,
            }
        };
        let bottom_padding = 1;
        let content_height = self.content.height(width.saturating_sub(role_width));
        content_height + bottom_padding
    }
}

// Delegate Content trait to its inner content.
impl Content for Message {
    fn height(&self, width: u16) -> usize {
        if let Some(thinking) = self.content.as_any().downcast_ref::<Thinking>()
            && thinking.is_collapsed()
        {
            return 0;
        }
        let role_width = match self.state.role {
            Role::User => 8,
            Role::Bot => 8,
            Role::System => 2, // margin width for system messages (no role prefix)
        };
        let bottom_padding = 1;
        let content_height = self.content.height(width.saturating_sub(role_width));
        content_height + bottom_padding
    }

    fn focus_range(&self, width: u16) -> Option<Range<u16>> {
        if let Some(thinking) = self.content.as_any().downcast_ref::<Thinking>()
            && thinking.is_collapsed()
        {
            return None;
        }
        let role_width = match self.state.role {
            Role::User => 8,
            Role::Bot => 8,
            Role::System => 2,
        };
        let content_width = width.saturating_sub(role_width);
        self.content.focus_range(content_width)
    }

    fn is_actionable(&self) -> bool {
        self.content.is_actionable()
    }

    fn shortcut_hints(&self) -> ShortcutHints {
        self.content.shortcut_hints()
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

    fn handle_navigation(&mut self, key: NavigationKey) -> NavigationResult {
        self.content.handle_navigation(key)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        self.draw_compact(frame, area, false)
    }
}

impl Message {
    pub fn draw_compact(&mut self, frame: &mut Frame, area: Rect, compact: bool) -> Result<()> {
        use Constraint::*;

        if let Some(thinking) = self.content.as_any().downcast_ref::<Thinking>()
            && thinking.is_collapsed()
        {
            return Ok(());
        }

        let area_content = if matches!(self.state.role, Role::System) || compact {
            area.inner(Margin {
                horizontal: 1,
                vertical: 0,
            })
        } else {
            let [area_role, area_content] = Layout::horizontal([Length(8), Min(1)]).areas(area);
            if self.state.show_role_prefix {
                let theme = global::theme();
                let is_thinking = self.is_thinking();
                let paragraph = Paragraph::new(Line::from(match self.state.role {
                    Role::User => Span::styled(" User: ", theme.ui.user_role),
                    Role::Bot if is_thinking => Span::styled(" Think: ", theme.ui.thinking_role),
                    Role::Bot => Span::styled(" Bot: ", theme.ui.bot_role),
                    _ => unreachable!(),
                }));
                frame.render_widget(paragraph, area_role);
            }
            area_content
        };
        self.content.draw(frame, area_content)?;

        Ok(())
    }
}

impl ContentComponent for Message {}
