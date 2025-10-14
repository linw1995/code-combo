use color_eyre::Result;
use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Frame, layout::Rect};
use tokio::sync::mpsc::UnboundedSender;

use crate::{actions::*, events::*};

/// A macro to handle component events in a standardized way.
///
/// This macro provides a consistent pattern for handling different types of events
/// (keyboard, mouse, and other events) for UI components. It matches on the event type
/// and calls the appropriate handler method on the component. For non-key/mouse events,
/// it propagates the event to all child components.
///
/// # Arguments
///
/// * `$component` - The component instance that will handle the event
/// * `$event` - The event to be processed
///
/// # Example
///
/// ```rust
/// handle_component_event!(self, event);
/// ```
///
/// # Why
///
/// This approach is preferred over a helper method in the trait to maintain clean and
/// organized trait methods.
macro_rules! handle_component_event {
    ($component:ident, $event:ident) => {
        match $event {
            Event::Key(key_event) => $component.handle_key_event(key_event),
            Event::Mouse(mouse_event) => $component.handle_mouse_event(mouse_event),
            _ => {
                for child in $component.children() {
                    child.handle_event($event);
                }
            }
        }
    };
}

mod chat;
mod input;
mod messages;

pub use chat::Chat;
pub use input::Input;
pub use messages::{Content, Message, Role};

/// `Component` is a trait that represents a visual and interactive element of the user interface.
#[allow(dead_code)]
pub trait Component {
    /// Get the children components of this component.
    ///
    /// This method returns an iterator over mutable references to the child components.
    /// By default, it returns an empty iterator, meaning the component has no children.
    /// Components that contain other components should override this method to return
    /// their children.
    ///
    /// # Returns
    ///
    /// * `Box<dyn Iterator<Item = &'_ mut dyn Component> + '_>` - An iterator over mutable references to child components.
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(std::iter::empty())
    }

    /// Configure the component with action and event senders.
    ///
    /// This method sets up the communication channels for the component and its children
    /// by configuring both action and event senders. It recursively calls config on all
    /// child components to ensure they have access to the communication channels.
    ///
    /// # Arguments
    ///
    /// * `action_tx` - An unbounded sender for sending actions to other components.
    /// * `event_tx` - An unbounded sender for sending events to other components.
    fn config(&mut self, action_tx: UnboundedSender<Action>, event_tx: UnboundedSender<Event>) {
        self.config_action_sender(action_tx.clone());
        self.config_event_sender(event_tx.clone());

        for child in self.children() {
            child.config(action_tx.clone(), event_tx.clone());
        }
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
        handle_component_event!(self, event);
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
