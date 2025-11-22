use code_combo::{TextEdit, ToolUse, tools::STR_REPLACE_TOOL_NAME};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    prelude::Rect,
    text::Text,
    widgets::{Block, Paragraph},
};

use crate::{
    actions::{Action, ToolAction},
    components::shortcuts_desc,
    error,
    events::{AskEvent, Event},
    global::{self, State},
};

use super::{Component, Content, ContentComponent};

pub struct StrReplace<'a> {
    tool_use: ToolUse,
    edit: Option<TextEdit>,
    appliable: State<bool>,
    widget: Paragraph<'a>,
}

const CONTEXT_RADIUS: usize = 3;

impl<'a> StrReplace<'a> {
    pub fn new(tool_use: &ToolUse) -> Self {
        Self {
            tool_use: tool_use.to_owned(),
            edit: None,
            appliable: State::default(),
            widget: Paragraph::new(Text::from(String::new())),
        }
    }

    pub fn update_text_edit(&mut self, edit: TextEdit) {
        let diff = edit.text_diff();

        let mut buf = vec![];
        diff.unified_diff()
            .context_radius(CONTEXT_RADIUS)
            .to_writer(&mut buf)
            .expect("failed to write unified diff into memory");

        let text = String::from_utf8_lossy(&buf).to_string();

        self.edit = Some(edit);
        self.widget = Paragraph::new(Text::from(text));
    }
}

impl Content for StrReplace<'_> {
    fn height(&self, width: u16) -> usize {
        self.widget.line_count(width)
    }

    fn is_actionable(&self) -> bool {
        self.appliable.get()
    }

    fn block_bottom_with_shortcuts_desc<'b>(&self, block: Block<'b>) -> Block<'b> {
        if self.appliable.get() {
            block
                .title_bottom(shortcuts_desc(&[("Apply", "CR")]))
                .title_bottom(shortcuts_desc(&[("Reject", "Esc")]))
        } else {
            block
        }
    }
}

impl Component for StrReplace<'_> {
    fn handle_key_event(&mut self, key: &KeyEvent) {
        if !self.appliable.read() {
            return;
        }

        let Some(edit) = &mut self.edit else {
            return;
        };

        let is_rejecting = match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Enter) => false,
            (KeyModifiers::NONE, KeyCode::Esc) => true,
            _ => {
                return;
            } // ignore
        };

        *self.appliable.write() = false;

        global::action_tx()
            .send(Action::Tool(ToolAction::ApplyTextEdit {
                id: self.tool_use.id.clone(),
                name: STR_REPLACE_TOOL_NAME.to_string(),
                edit: edit.clone(),
                context_radius: CONTEXT_RADIUS,
                hunk_idx: 0,
                is_rejecting,
            }))
            .unwrap();
    }

    fn handle_event(&mut self, event: &Event) {
        match event {
            Event::Ask(AskEvent::TextEdit { edit, .. }) => {
                self.update_text_edit(edit.clone());
                *self.appliable.write() = true;
            }
            _ => {
                handle_component_event!(self, event);
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> error::Result<()> {
        frame.render_widget(&self.widget, area);
        Ok(())
    }
}

impl ContentComponent for StrReplace<'static> {}
