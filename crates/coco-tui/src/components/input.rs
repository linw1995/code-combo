use color_eyre::eyre::Result;
use crossterm::event::KeyEvent;
use ratatui::{Frame, prelude::*, symbols::border, widgets::Block};

use super::{Action, Component};

#[derive(Default)]
#[allow(dead_code)]
pub struct Input<'a> {
    pub textarea: tui_textarea::TextArea<'a>,
}

impl Component for Input<'_> {
    fn handle_key_events(&mut self, key: KeyEvent) -> Action {
        self.textarea.input(key);
        Action::Noop
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let title = Line::from(" Input ".bold());
        let instructions = Line::from(vec![" Submit ".into(), "<Enter> ".blue().bold()]);
        let block = Block::bordered()
            .title(title.left_aligned())
            .title_bottom(instructions.left_aligned())
            .border_set(border::THICK);
        frame.render_widget(&self.textarea, block.inner(area));
        frame.render_widget(block, area);
        Ok(())
    }
}
