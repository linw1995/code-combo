use coco_macro::{ComponentExt, ContentComponentExt};
use code_combo::{ToolUse, tools::Final};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Frame, prelude::Rect, widgets::Wrap};
use serde::{Deserialize, Serialize};

use crate::{
    actions::ToolAction,
    components::{Component, Content, ContentComponent, Persistable},
    error::Result,
    events::{AnswerEvent, Event},
    global::{self, State},
    session::{self, Session},
    widgets::Paragraph,
};

#[derive(Serialize, Deserialize)]
struct Inner {
    tool_use: ToolUse,
    is_actionable: bool,
    output: Option<Final>,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "raw")]
pub struct Raw<'a> {
    state: State<Inner>,
    widget: Paragraph<'a>,
}

fn generate_widget<'b>(tool_use: &ToolUse, output: &Option<Final>) -> Paragraph<'b> {
    let mut text = match serde_json::to_string_pretty(&tool_use.input) {
        Ok(json_str) => format!("Input: {}", json_str),
        Err(_) => "Input: [Invalid JSON]".to_string(),
    };
    if let Some(output) = output {
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
    Paragraph::new_wrap(text, Wrap { trim: false })
}

impl<'a> Raw<'a> {
    pub fn new(tool_use: ToolUse) -> Self {
        let widget = generate_widget(&tool_use, &None);
        Self {
            state: State::new(Inner {
                tool_use,
                is_actionable: true,
                output: None,
            }),
            widget,
        }
    }

    pub fn new_readonly(tool_use: ToolUse) -> Self {
        let widget = generate_widget(&tool_use, &None);
        Self {
            state: State::new(Inner {
                tool_use,
                is_actionable: false,
                output: None,
            }),
            widget,
        }
    }
}

impl Persistable for Raw<'static> {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: Inner = session::load(session)?;
        let widget = generate_widget(&state.tool_use, &state.output);
        Ok(Self {
            state: State::new(state),
            widget,
        })
    }
}

impl Component for Raw<'static> {
    fn handle_event(&mut self, event: &Event) {
        if let Event::Answer(AnswerEvent::ToolResult { output, .. }) = event {
            let output = Some(output.to_owned());
            self.widget = generate_widget(&self.state.tool_use, &output);
            self.state.write().output = output;
        } else {
            handle_component_event!(self, event);
        }
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        let action = match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Enter) => {
                ToolAction::Grant(self.state.tool_use.to_owned())
            }
            (KeyModifiers::NONE, KeyCode::Esc) => {
                ToolAction::Cancel(self.state.tool_use.to_owned())
            }
            _ => {
                // ignore
                return;
            }
        };
        self.state.write().is_actionable = false;
        global::action_tx().send(action.into()).unwrap();
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        frame.render_widget(&self.widget, area);
        Ok(())
    }
}

impl<'a> Content for Raw<'a> {
    fn is_actionable(&self) -> bool {
        self.state.is_actionable
    }

    fn height(&self, width: u16) -> usize {
        self.widget.line_count(width)
    }
}

impl ContentComponent for Raw<'static> {}
