use std::cmp::min;

use coco_macro::ComponentExt;
use crossterm::event::KeyEvent;
use ratatui::{
    Frame,
    backend::TestBackend,
    layout::Flex,
    prelude::*,
    symbols::{border, line},
    widgets::{Block, Borders, Scrollbar, ScrollbarOrientation, ScrollbarState},
};
use snafu::ResultExt;
use tracing::{trace, warn};

use crate::{
    components::Persistable,
    error::*,
    global::{self, State},
    session::{self, Session},
};

use super::{AnswerEvent, AskEvent, Component, Content, Event, Message};

mod combo;
mod plain;
mod tool;
pub use combo::Combo;
pub use plain::Plain;
pub use tool::Tool;

#[derive(Default, ComponentExt)]
#[component(type_id = "messages")]
pub struct Messages {
    messages: State<Vec<Message>>,
    focus: State<Option<usize>>,

    // scrolling
    viewport_height: u16,
    /// Updated during rendering because it depends on viewport width
    total_height: u16,
    offset: State<u16>,
}

impl Messages {
    pub fn extend(&mut self, iter: impl Iterator<Item = Message>) {
        self.messages.write().extend(iter);
    }

    pub fn push(&mut self, message: Message) {
        self.messages.write().push(message);
    }

    /// Clear all messages
    pub fn clear(&mut self) {
        self.messages.write().clear();
        *self.focus.write() = None;
        *self.offset.write() = 0;
        self.total_height = 0;
    }

    /// Check if there are no messages
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    fn new_scrollstate(&self) -> ScrollbarState {
        // N is the viewport sliding range on the whole content.
        // It has double end, so we need to add 1 to fit with position design.
        // position = N - 1: thumb at bottom
        // position = 0: thumb at top
        let hiden_range = self.total_height - self.viewport_height;
        let position_range = hiden_range + 1;
        let position = (position_range - 1) - self.offset.get();
        trace!(?position_range, position, "print scroll state");
        ScrollbarState::new(position_range as usize).position(position as usize)
    }

    // TODO: Update focus if it's hidden while viewport changing
    pub fn scroll_half_up(&mut self) {
        self.scroll_up(self.viewport_height / 2);
    }

    pub fn scroll_half_down(&mut self) {
        self.scroll_down(self.viewport_height / 2);
    }

    pub fn scroll_up(&mut self, offset: u16) {
        *self.offset.write() = min(
            self.offset.get() + offset,
            self.total_height.saturating_sub(self.viewport_height),
        );
    }

    pub fn scroll_down(&mut self, offset: u16) {
        let mut value = self.offset.write();
        *value = value.saturating_sub(offset);
    }

    pub fn selected_idx(&self) -> Option<usize> {
        self.focus.get()
    }

    pub fn blur(&mut self) {
        *self.focus.write() = None
    }

    pub fn focus(&mut self, idx: usize) -> bool {
        if idx < self.messages.len() {
            *self.focus.write() = Some(idx);
            true
        } else {
            false
        }
    }

    pub fn select_prev(&mut self) -> bool {
        if self.messages.is_empty() {
            return false;
        }
        if let Some(idx) = self.focus.get()
            && idx > 0
        {
            *self.focus.write() = Some(idx - 1);
            return true;
        }
        false
    }

    pub fn select_next(&mut self) -> bool {
        if let Some(idx) = self.focus.get()
            && idx < self.messages.len() - 1
        {
            *self.focus.write() = Some(idx + 1);
            return true;
        }
        false
    }

    pub fn select_last(&mut self) -> bool {
        if self.messages.is_empty() {
            false
        } else {
            *self.focus.write() = Some(self.messages.len() - 1);
            true
        }
    }

    pub fn locate_tool_message(&mut self, id: &str) -> Option<usize> {
        if let Some((idx, _)) = self
            .messages
            .iter()
            .enumerate()
            .find(|(_, m)| m.is_same_tool_id(id))
        {
            Some(idx)
        } else {
            None
        }
    }

    /// Returns the index in the vector of the tool message that handled the event.
    pub fn on_tool_event(&mut self, event: &Event) -> Option<usize> {
        match event {
            Event::Ask(AskEvent::ToolUsePermission(id))
            | Event::Ask(AskEvent::TextEdit { id, .. })
            | Event::Answer(AnswerEvent::ToolResult { id, .. }) => {
                if let Some(idx) = self.locate_tool_message(id) {
                    // Pass through the relative event to its component.
                    self.messages.write_untracked()[idx].handle_event(event);
                    return Some(idx);
                }
            }
            _ => (),
        }
        None
    }

    fn draw_scrollbar(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some(line::VERTICAL))
            .track_style(Style::default().dark_gray())
            .thumb_symbol(line::THICK_VERTICAL);
        let mut state = self.new_scrollstate();
        frame.render_stateful_widget(bar, area, &mut state);
        Ok(())
    }

    fn virtual_draw(&mut self, frame: &mut Frame, area: Rect, heights: &[usize]) -> Result<()> {
        // FIXME: CPU intensive, try caching the virtual range result
        trace!(?self.viewport_height, ?self.total_height, ?self.offset, "virtual draw");

        let v_area = Rect::new(0, 0, area.width, self.total_height);
        let mem = TestBackend::new(v_area.width, v_area.height);
        let mut vtem = Terminal::new(mem).whatever_context("failed to new terminal")?;

        let completed_frame = vtem
            .draw(|frame| self.actual_draw(frame, v_area, heights).unwrap())
            .whatever_context("failed to draw terminal")?;

        let buf = frame.buffer_mut();
        let visible_content = completed_frame
            .buffer
            .content
            .iter()
            .skip(
                v_area.width as usize
                    * ((self.total_height - self.viewport_height - self.offset.get()) as usize),
            )
            .take(area.area() as usize);

        for (i, cell) in visible_content.enumerate() {
            let x = i as u16 % v_area.width;
            let y = i as u16 / v_area.width;
            buf[(area.x + x, area.y + y)] = cell.clone();
        }
        Ok(())
    }

    fn actual_draw(&mut self, frame: &mut Frame, area: Rect, heights: &[usize]) -> Result<()> {
        use Constraint::Length;

        let chunks = Layout::vertical(heights.iter().map(|h| Length(*h as u16)))
            .flex(Flex::End)
            .split(area);

        for (idx, message) in self.messages.write_untracked().iter_mut().enumerate() {
            let mut block = Block::new().borders(Borders::LEFT);
            block = if &Some(idx) == self.focus.read() {
                block.border_set(border::THICK)
            } else {
                block
                    .border_set(border::PLAIN)
                    .border_style(Style::default().dark_gray())
            };
            let rect = chunks[idx];
            message.draw(frame, block.inner(rect)).unwrap();
            frame.render_widget(&block, rect);
        }

        Ok(())
    }
}

impl Content for Messages {
    fn height(&self, _width: u16) -> usize {
        // Use the cached result
        self.total_height as usize
    }

    fn is_actionable(&self) -> bool {
        let Some(idx) = self.focus.get() else {
            return false;
        };
        let component = &self.messages.read()[idx];
        component.is_actionable()
    }

    fn block_with_shortcuts_desc<'a>(&self, mut block: Block<'a>) -> Block<'a> {
        if let Some(idx) = self.focus.get() {
            let component = &self.messages.read()[idx];
            if component.is_actionable() {
                block = component.block_with_shortcuts_desc(block);
            }
        }
        block
    }
}

impl Persistable for Messages {
    fn save(&self) -> Session {
        let messages = self.messages.iter().map(|m| (m.save())).collect::<Vec<_>>();
        session::save(messages)
    }

    fn load(session: Session) -> Result<Self> {
        let messages: Vec<Session> = session::load(session)?;
        let mut inst = Self::default();
        for message in messages {
            inst.push(Message::load(message)?);
        }
        Ok(inst)
    }
}

impl Component for Messages {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(
            self.messages
                .write_untracked()
                .iter_mut()
                .map(|m| m as &mut dyn Component),
        )
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        match (self.focus.read(), key.modifiers, key.code) {
            (Some(idx), _, _) if self.messages[*idx].is_actionable() => {
                self.messages.write_untracked()[*idx].handle_key_event(key);
            }
            (_, _, _) => {
                warn!(?key, ?self.focus, "unknown key event")
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::{Length, Min};

        let border_width = 1;
        let scrollbar_width = 2;
        let heights: Vec<_> = self
            .messages
            .iter()
            .map(|m| m.height(area.width - border_width - scrollbar_width))
            .collect();
        self.total_height = heights.iter().sum::<usize>() as u16;

        if self.total_height > area.height {
            let [area_list, area_bar] =
                Layout::horizontal([Min(10), Length(scrollbar_width)]).areas(area);
            trace!(?area_list, ?area_bar, "print messages area");

            // Store the actual height of viewport
            self.viewport_height = area.height;
            self.virtual_draw(frame, area_list, &heights)?;
            self.draw_scrollbar(frame, area_bar)?;
        } else {
            self.actual_draw(frame, area, &heights)?;
        }

        Ok(())
    }
}

pub(super) fn shortcuts_desc<'a>(pairs: &[(&str, &str)]) -> Line<'a> {
    let theme = global::theme();
    let descs: Vec<&str> = pairs.iter().map(|(desc, _)| desc.to_owned()).collect();
    let mut spans = vec![Span::styled(
        format!(" {} ", descs.join("/")),
        theme.ui.shortcut_desc,
    )];
    let last_idx = pairs.len() - 1;
    for (idx, (_, key)) in pairs.iter().enumerate() {
        spans.push(Span::styled(format!("<{key}>"), theme.ui.shortcut));
        if idx != last_idx {
            spans.push(Span::raw("/"));
        }
    }
    spans.push(" ".into());
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;

    use crate::global::theme;

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn simple_overflow() {
        let mut app = Messages::default();
        app.extend(
            [
                Message::user(Plain::new("Hello".to_string()).into()),
                Message::user(Plain::new("Hello world".to_string()).into()),
            ]
            .into_iter(),
        );

        let mut terminal = Terminal::new(TestBackend::new(17, 5)).unwrap();
        terminal
            .draw(|frame| app.draw(frame, frame.area()).unwrap())
            .unwrap();

        let mut expected = Buffer::with_lines(vec![
            "                 ",
            "                 ",
            "│ User:  Hello   ",
            "│ User:  Hello   ",
            "│        world   ",
        ]);
        let border_style = Style::new().dark_gray();
        expected.set_style(Rect::new(0, 2, 1, 3), border_style);
        let role_style = theme().ui.user_role;
        expected.set_style(Rect::new(1, 2, 7, 1), role_style);
        expected.set_style(Rect::new(1, 3, 7, 1), role_style);

        assert_eq!(terminal.backend().buffer(), &expected);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn vertical_overflow() {
        let mut app = Messages::default();
        app.extend(
            [
                Message::user(Plain::new("Hello".to_string()).into()),
                Message::user(Plain::new("Lorem ipsum dolor sit amet".to_string()).into()),
            ]
            .into_iter(),
        );

        let mut terminal = Terminal::new(TestBackend::new(17, 5)).unwrap();
        terminal
            .draw(|frame| app.draw(frame, frame.area()).unwrap())
            .unwrap();
        let mut expected = Buffer::with_lines(vec![
            "│ User:  Lorem  │",
            "│        ipsum  ┃",
            "│        dolor  ┃",
            "│        sit    ┃",
            "│        amet   ┃",
        ]);
        let border_style = Style::new().dark_gray();
        expected.set_style(Rect::new(0, 0, 1, 5), border_style);
        let scrollbar_style = Style::new().dark_gray();
        expected.set_style(Rect::new(16, 0, 1, 1), scrollbar_style);
        let role_style = theme().ui.user_role;
        expected.set_style(Rect::new(1, 0, 7, 1), role_style);
        assert_eq!(terminal.backend().buffer(), &expected);

        app.scroll_up(1);

        terminal
            .draw(|frame| app.draw(frame, frame.area()).unwrap())
            .unwrap();
        let mut expected = Buffer::with_lines(vec![
            "│ User:  Hello  ┃",
            "│ User:  Lorem  ┃",
            "│        ipsum  ┃",
            "│        dolor  ┃",
            "│        sit    │",
        ]);
        let border_style = Style::new().dark_gray();
        expected.set_style(Rect::new(0, 0, 1, 5), border_style);
        let scrollbar_style = Style::new().dark_gray();
        expected.set_style(Rect::new(16, 4, 1, 1), scrollbar_style);
        expected.set_style(Rect::new(1, 0, 7, 2), role_style);
        assert_eq!(terminal.backend().buffer(), &expected);

        app.scroll_up(1);

        terminal
            .draw(|frame| app.draw(frame, frame.area()).unwrap())
            .unwrap();
        assert_eq!(terminal.backend().buffer(), &expected);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn vertical_overflow_with_offset() {
        let mut app = Messages::default();
        app.extend(
            [
                Message::user(Plain::new("Hello".to_string()).into()),
                Message::user(Plain::new("Lorem ipsum dolor sit amet".to_string()).into()),
            ]
            .into_iter(),
        );

        let mut terminal = Terminal::new(TestBackend::new(18, 6)).unwrap();
        let offset_area = Rect::new(1, 1, 17, 5);
        terminal
            .draw(|frame| app.draw(frame, offset_area).unwrap())
            .unwrap();
        let mut expected = Buffer::with_lines(vec![
            "                  ",
            " │ User:  Lorem  │",
            " │        ipsum  ┃",
            " │        dolor  ┃",
            " │        sit    ┃",
            " │        amet   ┃",
        ]);
        let border_style = Style::new().dark_gray();
        expected.set_style(Rect::new(1, 1, 1, 5), border_style);
        let scrollbar_style = Style::new().dark_gray();
        expected.set_style(Rect::new(17, 1, 1, 1), scrollbar_style);
        let role_style = theme().ui.user_role;
        expected.set_style(Rect::new(2, 1, 7, 1), role_style);
        assert_eq!(terminal.backend().buffer(), &expected);

        app.scroll_up(1);

        terminal
            .draw(|frame| app.draw(frame, offset_area).unwrap())
            .unwrap();
        let mut expected = Buffer::with_lines(vec![
            "                  ",
            " │ User:  Hello  ┃",
            " │ User:  Lorem  ┃",
            " │        ipsum  ┃",
            " │        dolor  ┃",
            " │        sit    │",
        ]);
        let border_style = Style::new().dark_gray();
        expected.set_style(Rect::new(1, 1, 1, 5), border_style);
        let scrollbar_style = Style::new().dark_gray();
        expected.set_style(Rect::new(17, 5, 1, 1), scrollbar_style);
        expected.set_style(Rect::new(2, 1, 7, 2), role_style);
        assert_eq!(terminal.backend().buffer(), &expected);

        app.scroll_up(1);

        terminal
            .draw(|frame| app.draw(frame, offset_area).unwrap())
            .unwrap();
        assert_eq!(terminal.backend().buffer(), &expected);
    }
}
