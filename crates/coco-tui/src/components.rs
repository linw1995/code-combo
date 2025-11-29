use std::any::Any;

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Frame, layout::Rect};

use crate::{actions::*, error::*, events::*, session::Session};

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
    ($component:ident, $event:ident) => {{
        match $event {
            Event::Key(key_event) => $component.handle_key_event(key_event),
            Event::Mouse(mouse_event) => $component.handle_mouse_event(mouse_event),
            _ => {
                // Trigger on_tick and propagate the tick event to all children.
                if matches!($event, Event::Tick) {
                    $component.on_tick();
                } else {
                    tracing::trace!(?$event, "handling component event");
                }
                for child in $component.children() {
                    child.handle_event($event);
                }
            }
        }
    }};
}

mod chat;
mod code_highlight;
mod input;
mod message;
mod messages;

pub use chat::Chat;
pub use code_highlight::CodeHighlight;
pub use input::Input;
pub use message::*;
pub use messages::*;

/// Provides a unique identifier string for struct registry.
///
/// This trait allows components to be uniquely identified, which is useful for
/// registration systems, session management, and component lookup operations.
/// Each implementing struct should return a constant string that uniquely
/// identifies its type.
///
/// The `coco-macro` crate provides a derive macro `ComponentExt` that will fill the implementation.
pub trait Identity {
    /// Get the unique identifier string for this struct.
    ///
    /// # Returns
    ///
    /// * `&'static str` - A static string slice that uniquely identifies this type
    fn id(&self) -> &'static str;
}

/// Provides persistence capabilities for components.
///
/// This trait defines the interface for components that can save their state to persistent storage
/// and restore their state from previously saved data. This enables persistence
/// of component state across application restarts.
pub trait Persistable: Identity {
    /// Save the current state of the component to persistent storage.
    ///
    /// # Returns
    ///
    /// * `Session` - A session object containing the serialized state of the component.
    fn save(&self) -> Session;

    /// Load the component state from previously saved data.
    ///
    /// # Arguments
    ///
    /// * `state` - The session state to restore from
    ///
    /// # Returns
    ///
    /// * `Result<Self>` - The restored component instance or an error if loading fails
    ///
    /// # Type Parameters
    ///
    /// * `Self` - The implementing component type
    fn load(state: Session) -> Result<Self>
    where
        Self: Sized;
}

/// `Component` is a trait that represents a visual and interactive element of the user interface.
pub trait Component: Persistable + Any + Send {
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

    /// Handle init event.
    fn on_init(&mut self) {}

    /// Handle tick events.
    fn on_tick(&mut self) {}

    /// Handle incoming actions and proprogate action if necessary.
    ///
    /// # Arguments
    ///
    /// * `action` - An action to be processed.
    fn handle_action(&mut self, action: &Action) {
        self.update(action);
        for child in self.children() {
            child.handle_action(action);
        }
    }

    /// Update the state of the component based on a received action.
    ///
    /// # Arguments
    ///
    /// * `action` - An action that may modify the state of the component.
    #[allow(unused_variables)]
    fn update(&mut self, action: &Action) {}

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

impl<T: Component> From<T> for Box<dyn Component> {
    fn from(value: T) -> Self {
        Box::new(value)
    }
}

impl dyn Component {
    /// Allow downcasting the trait object to its concrete type at runtime
    pub fn as_any(&self) -> &dyn Any {
        self
    }

    /// Allow downcasting the trait object to its concrete type at runtime
    pub fn as_mut_any(&mut self) -> &mut dyn Any {
        self
    }
}
