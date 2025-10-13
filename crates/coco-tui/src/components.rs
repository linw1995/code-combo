use color_eyre::Result;
use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Frame, layout::Rect};
use tokio::sync::mpsc::UnboundedSender;

use crate::{actions::Action, events::Event};

mod chat;
mod input;
mod messages;

pub use chat::Chat;
pub use input::Input;
pub use messages::{Content, Message, Role};

/// `Component` is a trait that represents a visual and interactive element of the user interface.
#[allow(dead_code)]
pub trait Component {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(std::iter::empty())
    }
    /// Configure the action sender for the component.
    ///
    /// This method is called only once during component initialization to set up
    /// the communication channel for sending actions.
    ///
    /// # Arguments
    ///
    /// * `tx` - An unbounded sender that can be used to send actions to other components.
    #[allow(unused_variables)]
    fn config_action_sender(&mut self, tx: UnboundedSender<Action>) {}
    /// Configure the event sender for the component.
    ///
    /// This method is called only once during component initialization to set up
    /// the communication channel for sending events.
    ///
    /// # Arguments
    ///
    /// * `tx` - An unbounded sender that can be used to send events to other components.
    #[allow(unused_variables)]
    fn config_event_sender(&mut self, tx: UnboundedSender<Event>) {}
    /// Handle incoming events and proprogate events if necessary.
    ///
    /// # Arguments
    ///
    /// * `event` - An event to be processed.
    fn handle_event(&mut self, event: &Event) {
        match event {
            Event::Key(key_event) => self.handle_key_event(key_event),
            Event::Mouse(mouse_event) => self.handle_mouse_event(mouse_event),
            _ => {
                for child in self.children() {
                    child.handle_event(event);
                }
            }
        }
    }
    /// Handle key events.
    ///
    /// # Arguments
    ///
    /// * `key` - A key event to be processed.
    #[allow(unused_variables)]
    fn handle_key_event(&mut self, key: &KeyEvent) {}
    /// Handle mouse events.
    ///
    /// # Arguments
    ///
    /// * `mouse` - A mouse event to be processed.
    #[allow(unused_variables)]
    fn handle_mouse_event(&mut self, mouse: &MouseEvent) {}

    /// Handle incoming actions and proprogate action if necessary.
    ///
    /// # Arguments
    ///
    /// * `action` - An action to be processed.
    fn handle_action(&mut self, action: &Action) {
        self.update(action);
        for child in self.children() {
            child.update(action);
        }
    }
    /// Update the state of the component based on a received action. (REQUIRED)
    ///
    /// # Arguments
    ///
    /// * `action` - An action that may modify the state of the component.
    #[allow(unused_variables)]
    fn update(&mut self, action: &Action);
    /// Render the component on the screen. (REQUIRED)
    ///
    /// # Arguments
    ///
    /// * `frame` - A frame used for rendering.
    /// * `area` - The area in which the component should be drawn.
    ///
    /// # Returns
    ///
    /// * `Result<()>` - An Ok result or an error.
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()>;
}
