use std::any::Any;

use color_eyre::Result;
use crossterm::event::KeyEvent;
use ratatui::{
    Frame,
    layout::Flex,
    prelude::*,
    symbols::border,
    widgets::{Block, Borders, Paragraph},
};
use tracing::warn;

use super::{AnswerEvent, AskEvent, Component, Event};

mod combo;
mod plain;
mod tool;
pub use combo::Combo;
pub use plain::Plain;
pub use tool::Tool;

#[derive(Default)]
pub struct Messages {
    messages: Vec<Message>,
    focus: Option<usize>,
}

impl Messages {
    pub fn extend(&mut self, iter: impl Iterator<Item = Message>) {
        self.messages.extend(iter);
    }

    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn selected_idx(&self) -> Option<usize> {
        self.focus
    }

    pub fn blur(&mut self) {
        self.focus = None
    }

    pub fn focus(&mut self, idx: usize) -> bool {
        if idx < self.messages.len() {
            self.focus = Some(idx);
            true
        } else {
            false
        }
    }

    pub fn select_prev(&mut self) -> bool {
        if self.messages.is_empty() {
            return false;
        }
        if let Some(idx) = self.focus
            && idx > 0
        {
            self.focus = Some(idx - 1);
            return true;
        }
        false
    }

    pub fn select_next(&mut self) -> bool {
        if let Some(idx) = self.focus
            && idx < self.messages.len() - 1
        {
            self.focus = Some(idx + 1);
            return true;
        }
        false
    }

    pub fn select_last(&mut self) -> bool {
        if self.messages.is_empty() {
            false
        } else {
            self.focus = Some(self.messages.len() - 1);
            true
        }
    }

    pub fn locate_tool_message(&mut self, id: &str) -> Option<usize> {
        if let Some((idx, _)) = self.messages.iter().enumerate().find(|(_, m)| {
            m.content
                .as_any()
                .downcast_ref::<Tool>()
                .map(|tool| tool.id == id)
                .unwrap_or_default()
        }) {
            Some(idx)
        } else {
            None
        }
    }

    /// Returns the index in the vector of the tool message that handled the event.
    pub fn on_tool_event(&mut self, event: &Event) -> Option<usize> {
        match event {
            Event::Ask(AskEvent::ToolUsePermission(id))
            | Event::Answer(AnswerEvent::ToolResult { id, .. }) => {
                if let Some(idx) = self.locate_tool_message(id) {
                    // Pass through the relative event to its component.
                    self.messages[idx].handle_event(event);
                    return Some(idx);
                }
            }
            _ => (),
        }
        None
    }
}

impl Content for Messages {
    fn height(&self, _width: u16) -> usize {
        0
    }

    fn block_bottom_with_shortcuts_desc<'a>(&self, mut block: Block<'a>) -> Block<'a> {
        if let Some(idx) = self.focus {
            let component = &self.messages[idx];
            if component.is_actionable() {
                block = component.block_bottom_with_shortcuts_desc(block);
            }
        }
        block
    }
}

impl Component for Messages {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(self.messages.iter_mut().map(|m| m as &mut dyn Component))
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        match (&self.focus, key.modifiers, key.code) {
            (Some(idx), _, _) if self.messages[*idx].is_actionable() => {
                self.messages[*idx].handle_key_event(key);
            }
            (_, _, _) => {
                warn!(?key, ?self.focus, "unknown key event")
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::Length;

        let chunks = Layout::vertical(
            self.messages
                .iter()
                .map(|m| Length(m.height(area.width) as u16)),
        )
        .flex(Flex::End)
        .split(area);

        for (idx, message) in self.messages.iter_mut().enumerate() {
            let mut block = Block::new().borders(Borders::LEFT);
            block = if Some(idx) == self.focus {
                block.border_set(border::THICK)
            } else {
                block
                    .border_set(border::PLAIN)
                    .border_style(Style::default().dark_gray())
            };
            let rect = chunks[idx];
            frame.render_widget(&block, rect);
            message.draw(frame, block.inner(rect)).unwrap();
        }

        Ok(())
    }
}

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
    fn height(&self, width: u16) -> usize {
        self.content.height(width)
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
