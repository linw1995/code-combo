use color_eyre::eyre::Result;
use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};

use super::{Action, Component};

use super::Input;

#[derive(Default)]
pub struct Chat<'a> {
    pub input: Input<'a>,
}

impl Component for Chat<'_> {
    fn handle_key_events(&mut self, key: KeyEvent) -> Action {
        self.input.handle_key_events(key)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::{Length, Min};

        let vertical = Layout::vertical([Min(0), Length(3)]);
        let [_main_area, input_area] = vertical.areas(area);

        self.input.draw(frame, input_area)
    }
}
