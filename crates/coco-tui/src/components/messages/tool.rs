use code_combo::ToolUse;
use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Alignment,
    prelude::Rect,
    style::Stylize,
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use serde_json::Value;
use tracing::debug;

use super::{Component, Content, ContentComponent};
use crate::{
    actions::ToolAction,
    components::shortcuts_desc,
    events::{AnswerEvent, AskEvent, Event},
    global::{self, State},
};

#[derive(Debug, Clone, Default, PartialEq)]
pub enum ToolState {
    #[default]
    Initing,
    PendingConfirmation,
    Cancelled,

    Executing,

    Completed,
    Failed,
}

pub struct Tool {
    pub id: String,
    pub name: String,
    pub input: Value,
    pub state: State<ToolState>,
    pub output: State<Option<Value>>,
}

// TODO: Allow user to edit tool input parameters

impl Tool {
    pub fn new_use(id: String, name: String, input: Value) -> Self {
        Self {
            id,
            name,
            input,
            state: State::default(),
            output: State::default(),
        }
    }

    pub fn update_state(&mut self, new_state: ToolState) {
        let state = self.state.read();
        if state == &new_state {
            return;
        }
        debug!(?state, ?new_state, "update state");
        *self.state.write() = new_state
    }

    fn get_title_spans(&self) -> Vec<Span<'_>> {
        let mut spans = vec![
            " 󱁤  ".blue(),
            "Tool: ".into(),
            self.name.as_str().cyan(),
            " ".into(),
        ];
        match self.state.read() {
            ToolState::Initing => spans.push("  Initing...".yellow()),
            ToolState::PendingConfirmation => {
                spans.push("  Awaiting confirmation".blue());
            }
            ToolState::Executing => {
                spans.push("  Executing...".yellow());
            }
            ToolState::Completed => {
                spans.push("  Completed".green());
            }
            ToolState::Failed => {
                spans.push("  Failed".red());
            }
            ToolState::Cancelled => {
                spans.push("  Cancelled".red());
            }
        }
        spans.push(" ".into());
        spans
    }

    fn tool(&self) -> ToolUse {
        ToolUse {
            id: self.id.clone(),
            name: self.name.clone(),
            input: self.input.clone(),
        }
    }

    fn get_content_text(&self) -> String {
        let mut text = match serde_json::to_string_pretty(&self.input) {
            Ok(json_str) => format!("Input: {}", json_str),
            Err(_) => "Input: [Invalid JSON]".to_string(),
        };
        if let Some(output) = self.output.read() {
            let output = match serde_json::to_string_pretty(output) {
                Ok(json_str) => format!("Output: {}", json_str),
                Err(_) => "Output: [Invalid JSON]".to_string(),
            };
            text.push('\n');
            text.push_str(&output);
        }
        text
    }
}

impl Component for Tool {
    fn handle_event(&mut self, event: &Event) {
        match event {
            Event::Ask(AskEvent::ToolUsePermission(id)) => {
                if &self.id == id {
                    self.update_state(ToolState::PendingConfirmation);
                }
            }
            Event::Answer(AnswerEvent::ToolResult {
                id,
                is_error,
                output,
            }) => {
                if &self.id == id {
                    self.update_state(if *is_error {
                        ToolState::Failed
                    } else {
                        ToolState::Completed
                    });
                    *self.output.write() = Some(output.to_owned());
                }
            }
            _ => handle_component_event!(self, event),
        }
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Enter) => {
                global::action_tx()
                    .send(ToolAction::Grant(self.tool()).into())
                    .unwrap();
                self.update_state(ToolState::Executing);
            }
            (KeyModifiers::NONE, KeyCode::Esc) => {
                global::action_tx()
                    .send(ToolAction::Cancel(self.tool()).into())
                    .unwrap();
                self.update_state(ToolState::Cancelled);
            }
            _ => (), // ignore
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        // Create block with title containing tool name and status
        let title_spans = self.get_title_spans();
        let block = Block::new()
            .borders(Borders::TOP)
            .border_set(border::THICK)
            .title(Line::from("")) // placeholder for border on the left of the actual title
            .title(Line::from(title_spans))
            .title_alignment(Alignment::Left);

        // Get content area inside the block
        let content_area = block.inner(area);

        // Create content paragraph
        let content = self.get_content_text();
        let content = Paragraph::new(content).wrap(Wrap { trim: false });

        // Render the block and content
        frame.render_widget(&block, area);
        frame.render_widget(content, content_area);

        Ok(())
    }
}

impl Content for Tool {
    fn height(&self, width: u16) -> usize {
        // Base height for title
        let base_height = 1;
        base_height
            + Paragraph::new(self.get_content_text())
                .wrap(Wrap { trim: false })
                .line_count(width)
    }

    fn is_actionable(&self) -> bool {
        self.state.read() == &ToolState::PendingConfirmation
    }

    fn block_bottom_with_shortcuts_desc<'a>(&self, block: Block<'a>) -> Block<'a> {
        block
            .title_bottom(shortcuts_desc(&[("Ok", "CR")]))
            .title_bottom(shortcuts_desc(&[("Cancel", "Esc")]))
    }
}

impl ContentComponent for Tool {}
