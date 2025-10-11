use color_eyre::Result;

use ratatui::{Frame, layout::Rect};

mod input;

/// `Component` is a trait that represents a visual and interactive element of the user interface.
#[allow(dead_code)]
pub trait Component {
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
