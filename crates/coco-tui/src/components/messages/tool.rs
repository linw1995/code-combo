use coco_macro::{ComponentExt, ContentComponentExt};
use code_combo::{
    ToolUse,
    tools::{
        BASH_TOOL_NAME, LIST_TOOL_NAME, READ_TOOL_NAME, RUN_TASK_TOOL_NAME, STR_REPLACE_TOOL_NAME,
    },
};
use crossterm::event::KeyEvent;
use ratatui::{
    Frame,
    layout::Alignment,
    prelude::Rect,
    style::Style,
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, warn};

use crate::{
    actions::{Action, ToolAction},
    components::{Component, Content, ContentComponent, Persistable, ShortcutHints},
    error::*,
    events::{AnswerEvent, AskEvent, Event},
    global::{self, State},
    session::{self, Session},
    theme::FinalizedTheme,
};

mod bash;
mod list;
mod raw;
mod read;
mod run_task;
mod str_replace;
use bash::Bash;
use list::List;
use raw::Raw;
use read::Read;
use run_task::RunTask;
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
    is_focused: bool,
}

// TODO: Allow user to edit tool input parameters

impl Tool {
    pub fn new(tool_use: ToolUse) -> Self {
        let widget = match tool_use.name.as_str() {
            READ_TOOL_NAME => Some(Read::new(&tool_use).into()),
            LIST_TOOL_NAME => Some(List::new(&tool_use).into()),
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
            RUN_TASK_TOOL_NAME => match RunTask::try_new().tool_use(&tool_use).call() {
                Ok(widget) => Some(widget.into()),
                Err(err) => {
                    warn!(
                        ?err,
                        "failed to create RunTask component, falling back to default"
                    );
                    None
                }
            },
            _ => None,
        }
        .unwrap_or_else(|| Raw::new(tool_use.clone()).into());

        Self {
            inner: State::new(Inner {
                tool_use,
                state: ToolState::default(),
            }),
            widget,
            is_focused: false,
        }
    }

    pub fn mark_completed(&mut self) {
        self.update_state(ToolState::Completed);
    }

    pub fn mark_failed(&mut self) {
        self.update_state(ToolState::Failed);
    }

    pub fn tool_use_id(&self) -> &str {
        &self.inner.tool_use.id
    }

    pub fn tool_use_name(&self) -> &str {
        &self.inner.tool_use.name
    }

    pub fn is_pending_confirmation(&self) -> bool {
        matches!(self.inner.state, ToolState::PendingConfirmation)
    }

    pub fn is_running(&self) -> bool {
        matches!(self.inner.state, ToolState::Initing | ToolState::Executing)
    }

    pub fn update_state(&mut self, new_state: ToolState) {
        let state = &self.inner.state;
        if state == &new_state {
            return;
        }
        debug!(?state, ?new_state, "update state");
        self.inner.write().state = new_state
    }

    fn title_case_snake_case(s: &str) -> String {
        s.split('_')
            .map(Self::capitalize_first_ascii)
            .fold(String::new(), |part, mut all| {
                all.push_str(&part);
                all
            })
    }

    fn capitalize_first_ascii(s: &str) -> String {
        let mut bytes = s.as_bytes().to_vec();
        if let Some(b) = bytes.first_mut() {
            b.make_ascii_uppercase();
        }
        String::from_utf8(bytes).unwrap()
    }

    fn get_title_spans(&self, theme: &FinalizedTheme) -> Vec<Span<'_>> {
        let apply_dim = |style: Style| {
            if self.is_focused {
                style
            } else {
                style.patch(theme.ui.tool_title_dim)
            }
        };

        let (state_text, state_style) = {
            use ToolState::*;
            match self.inner.state {
                Initing => ("Initing...", theme.ui.tool_title_state_initing),
                PendingConfirmation => (
                    "Awaiting confirmation",
                    theme.ui.tool_title_state_pending_confirmation,
                ),
                Executing => ("Executing...", theme.ui.tool_title_state_executing),
                Completed => ("Completed", theme.ui.tool_title_state_completed),
                Failed => ("Failed", theme.ui.tool_title_state_failed),
                Cancelled => ("Cancelled", theme.ui.tool_title_state_cancelled),
            }
        };

        let mut spans = vec![
            Span::styled(
                format!(
                    " {} ",
                    Self::title_case_snake_case(&self.inner.tool_use.name)
                ),
                apply_dim(theme.ui.tool_title_name),
            ),
            Span::styled(state_text, apply_dim(state_style)),
        ];
        if matches!(self.inner.state, ToolState::Completed | ToolState::Failed)
            && let Some(line) = self.widget.reminder_line()
        {
            spans.extend(line.spans.into_iter().map(|mut span| {
                span.style = apply_dim(span.style);
                span
            }));
        }
        spans.push(Span::raw(" "));
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
            LIST_TOOL_NAME => List::load(child)?.into(),
            STR_REPLACE_TOOL_NAME => StrReplace::load(child)?.into(),
            RUN_TASK_TOOL_NAME => RunTask::load(child)?.into(),
            _ => Raw::load(child)?.into(),
        };
        Ok(Self {
            inner: State::new(inner),
            widget,
            is_focused: false,
        })
    }
}

impl Component for Tool {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(vec![self.widget.as_mut() as &mut dyn Component].into_iter())
    }

    fn handle_event(&mut self, event: &Event) {
        match event {
            Event::Ask(AskEvent::ToolUsePermission(id)) => {
                if self.tool_use_id() == id {
                    debug!(?event, "state change to pending confirmation");
                    self.update_state(ToolState::PendingConfirmation);
                    handle_component_event!(self, event);
                }
            }
            Event::Ask(AskEvent::TextEdit {
                id, auto_accept, ..
            }) => {
                if self.tool_use_id() == id {
                    if *auto_accept {
                        self.update_state(ToolState::Executing);
                    } else {
                        debug!(?event, "state change to pending confirmation");
                        self.update_state(ToolState::PendingConfirmation);
                    }
                    handle_component_event!(self, event);
                }
            }
            Event::Answer(AnswerEvent::ToolOutput { id, .. })
            | Event::Answer(AnswerEvent::SubagentEvent { id, .. }) => {
                if self.tool_use_id() == id {
                    // Auto-allowed tools skip permission, transition directly to Executing
                    if self.inner.state == ToolState::Initing {
                        self.update_state(ToolState::Executing);
                    }
                    handle_component_event!(self, event);
                }
            }
            Event::Answer(AnswerEvent::ToolResult {
                id,
                is_error,
                is_user_cancelled,
                ..
            }) => {
                if self.tool_use_id() == id {
                    let state = if *is_user_cancelled {
                        ToolState::Cancelled
                    } else if *is_error {
                        ToolState::Failed
                    } else {
                        ToolState::Completed
                    };
                    self.update_state(state);
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
            Action::Focus => {
                self.is_focused = true;
            }
            Action::Blur => {
                self.is_focused = false;
            }
            Action::Tool(ToolAction::Grant(ToolUse { id, .. })) => {
                if self.tool_use_id() == id {
                    self.update_state(ToolState::Executing);
                }
            }
            Action::Tool(ToolAction::GrantSession(ToolUse { id, .. })) => {
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
        let theme = global::theme();
        let title_spans = self.get_title_spans(theme);
        let mut block = Block::new()
            .borders(Borders::TOP)
            .title(Line::from("")) // placeholder for border on the left of the actual title
            .title(Line::from(title_spans))
            .title_alignment(Alignment::Left);
        block = if self.is_focused {
            block
                .border_set(border::THICK)
                .border_style(theme.ui.block_border_active)
        } else {
            block
                .border_set(border::PLAIN)
                .border_style(theme.ui.block_border_inactive)
        };

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

    fn shortcut_hints(&self) -> ShortcutHints {
        self.widget.shortcut_hints()
    }
}

impl ContentComponent for Tool {}
