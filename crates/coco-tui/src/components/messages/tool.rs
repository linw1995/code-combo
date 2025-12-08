use coco_macro::{ComponentExt, ContentComponentExt};
use code_combo::{
    ToolUse,
    tools::{BASH_TOOL_NAME, READ_TOOL_NAME, STR_REPLACE_TOOL_NAME},
};
use crossterm::event::KeyEvent;
use ratatui::{
    Frame,
    layout::Alignment,
    prelude::Rect,
    style::Stylize,
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::whatever;
use tracing::{debug, warn};

use crate::{
    actions::{Action, ToolAction},
    components::{Component, Content, ContentComponent, Persistable},
    error::*,
    events::{AnswerEvent, AskEvent, Event},
    global::State,
    session::{self, Session},
};

mod bash;
mod raw;
mod read;
mod str_replace;
use bash::Bash;
use raw::Raw;
use read::Read;
use str_replace::StrReplace;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum ToolState {
    #[default]
    Initing,
    PendingConfirmation,
    Cancelled,

    Executing,

    Completed,
    Failed,
}

#[derive(Serialize, Deserialize)]
struct Inner {
    tool_use: ToolUse,
    state: ToolState,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "tool")]
pub struct Tool {
    inner: State<Inner>,
    widget: Box<dyn ContentComponent>,
}

// TODO: Allow user to edit tool input parameters

impl Tool {
    pub fn new(tool_use: ToolUse) -> Self {
        let widget = match tool_use.name.as_str() {
            READ_TOOL_NAME => Some(Read::new(&tool_use).into()),
            BASH_TOOL_NAME => match Bash::try_new().tool_use(&tool_use).call() {
                Ok(widget) => Some(widget.into()),
                Err(err) => {
                    warn!(
                        ?err,
                        "failed to create Bash component, falling back to default"
                    );
                    None
                }
            },
            STR_REPLACE_TOOL_NAME => Some(StrReplace::new(&tool_use).into()),
            _ => None,
        }
        .unwrap_or_else(|| Raw::new(tool_use.clone()).into());

        Self {
            inner: State::new(Inner {
                tool_use,
                state: ToolState::default(),
            }),
            widget,
        }
    }

    pub fn tool_use_id(&self) -> &str {
        &self.inner.tool_use.id
    }

    pub fn update_state(&mut self, new_state: ToolState) {
        let state = &self.inner.state;
        if state == &new_state {
            return;
        }
        debug!(?state, ?new_state, "update state");
        self.inner.write().state = new_state
    }

    fn get_title_spans(&self) -> Vec<Span<'_>> {
        let mut spans = vec![
            " 󱁤  ".blue(),
            "Tool: ".into(),
            self.inner.tool_use.name.clone().cyan(),
        ];
        match self.inner.state {
            ToolState::Initing => spans.push("   Initing...".yellow()),
            ToolState::PendingConfirmation => {
                spans.push("   Awaiting confirmation".blue());
            }
            ToolState::Executing => {
                spans.push("   Executing...".yellow());
            }
            ToolState::Completed => {
                spans.push("   Completed".green());
            }
            ToolState::Failed => {
                spans.push("   Failed".red());
            }
            ToolState::Cancelled => {
                spans.push("   Cancelled".red());
            }
        }
        spans.push(" ".into());
        spans
    }
}

impl Persistable for Tool {
    fn save(&self) -> Session {
        session::save_related(&self.inner, self.widget.save())
    }

    fn load(session: Session) -> Result<Self> {
        let (inner, child): (Inner, Value) = session::load_related(session)?;
        let name = &inner.tool_use.name;
        let widget = match name.as_str() {
            BASH_TOOL_NAME => Bash::load(child)?.into(),
            READ_TOOL_NAME => Read::load(child)?.into(),
            STR_REPLACE_TOOL_NAME => StrReplace::load(child)?.into(),
            _ => whatever!("Unknown tool {name:?}"),
        };
        Ok(Self {
            inner: State::new(inner),
            widget,
        })
    }
}

impl Component for Tool {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(vec![self.widget.as_mut() as &mut dyn Component].into_iter())
    }

    fn handle_event(&mut self, event: &Event) {
        match event {
            Event::Ask(AskEvent::ToolUsePermission(id) | AskEvent::TextEdit { id, .. }) => {
                if self.tool_use_id() == id {
                    debug!(?event, "state change to pending confirmation");
                    self.update_state(ToolState::PendingConfirmation);
                    handle_component_event!(self, event);
                }
            }
            Event::Answer(AnswerEvent::ToolResult { id, is_error, .. }) => {
                if self.tool_use_id() == id {
                    self.update_state(if *is_error {
                        ToolState::Failed
                    } else {
                        ToolState::Completed
                    });
                    handle_component_event!(self, event);
                }
            }
            _ => handle_component_event!(self, event),
        }
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        self.widget.handle_key_event(key);
    }

    fn update(&mut self, action: &Action) {
        match action {
            Action::Tool(ToolAction::Grant(ToolUse { id, .. })) => {
                if self.tool_use_id() == id {
                    self.update_state(ToolState::Executing);
                }
            }
            Action::Tool(ToolAction::ApplyTextEdit {
                id, is_rejecting, ..
            }) => {
                if self.tool_use_id() == id {
                    if *is_rejecting {
                        self.update_state(ToolState::Cancelled);
                    } else {
                        self.update_state(ToolState::Executing);
                    }
                }
            }
            Action::Tool(ToolAction::Cancel(ToolUse { id, .. })) => {
                if self.tool_use_id() == id {
                    self.update_state(ToolState::Cancelled);
                }
            }
            _ => (),
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
        frame.render_widget(&block, area);
        let content_area = block.inner(area);

        // Create content paragraph
        self.widget.draw(frame, content_area)?;

        Ok(())
    }
}

impl Content for Tool {
    fn height(&self, width: u16) -> usize {
        // Base height for title
        let base_height = 1;
        base_height + self.widget.height(width)
    }

    fn is_actionable(&self) -> bool {
        self.widget.is_actionable()
    }

    fn block_with_shortcuts_desc<'a>(&self, block: Block<'a>) -> Block<'a> {
        self.widget.block_with_shortcuts_desc(block)
    }
}

impl ContentComponent for Tool {}
