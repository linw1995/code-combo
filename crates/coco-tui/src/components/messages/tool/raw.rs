use code_combo::{ToolUse, tools::Final};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    prelude::Rect,
    widgets::{Paragraph, Wrap},
};

use crate::{
    actions::ToolAction,
    components::{Component, Content, ContentComponent},
    error,
    events::{AnswerEvent, Event},
    global::{self, State},
};

pub struct Raw {
    tool_use: ToolUse,
    is_actionable: State<bool>,
    output: State<Option<Final>>,
}

impl Raw {
    pub fn new(tool_use: ToolUse) -> Self {
        Self {
            tool_use,
            is_actionable: State::new(true),
            output: State::default(),
        }
    }

    fn generate_default(&self) -> Paragraph<'_> {
        let mut text = match serde_json::to_string_pretty(&self.tool_use.input) {
            Ok(json_str) => format!("Input: {}", json_str),
            Err(_) => "Input: [Invalid JSON]".to_string(),
        };
        if let Some(output) = self.output.read() {
            let output = match output {
                Final::Json(output) => match serde_json::to_string_pretty(output) {
                    Ok(json_str) => format!("Output: {}", json_str),
                    Err(_) => "Output: [Invalid JSON]".to_string(),
                },
                Final::Message(text) => text.to_owned(),
            };
            text.push('\n');
            text.push_str(&output);
        }
        Paragraph::new(text).wrap(Wrap { trim: false })
    }
}

impl Component for Raw {
    fn handle_event(&mut self, event: &Event) {
        if let Event::Answer(AnswerEvent::ToolResult { output, .. }) = event {
            *self.output.write() = Some(output.to_owned());
        } else {
            handle_component_event!(self, event);
        }
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        let action = match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Enter) => ToolAction::Grant(self.tool_use.to_owned()),
            (KeyModifiers::NONE, KeyCode::Esc) => ToolAction::Cancel(self.tool_use.to_owned()),
            _ => {
                // ignore
                return;
            }
        };
        *self.is_actionable.write() = false;
        global::action_tx().send(action.into()).unwrap();
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> error::Result<()> {
        let content = self.generate_default();
        frame.render_widget(content, area);
        Ok(())
    }
}

impl Content for Raw {
    fn is_actionable(&self) -> bool {
        self.is_actionable.get()
    }

    fn height(&self, width: u16) -> usize {
        self.generate_default().line_count(width)
    }
}

impl ContentComponent for Raw {}
