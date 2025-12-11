use coco_macro::{ComponentExt, ContentComponentExt};
use code_combo::{
    TextEdit, ToolUse,
    tools::{Final, STR_REPLACE_TOOL_NAME},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Frame, prelude::Rect, style::Stylize, text::Text, widgets::Block};
use serde::{Deserialize, Serialize};
use snafu::prelude::*;
use tracing::warn;

use super::{Component, Content, ContentComponent};
use crate::{
    actions::{Action, ToolAction},
    components::{Persistable, code_highlight::CodeHighlight, shortcuts_desc},
    error::Result,
    events::{AnswerEvent, AskEvent, Event},
    global::{self, State},
    session::{self, Session},
    widgets::Paragraph,
};

#[derive(Serialize, Deserialize)]
struct Inner {
    tool_use: ToolUse,
    edit: Option<TextEdit>,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "str_replace")]
pub struct StrReplace<'a> {
    state: State<Inner>,
    widget: StrReplaceWidget<'a>,
}

enum StrReplaceWidget<'a> {
    CodeHighlight(CodeHighlight<'a>),
    Paragraph(Paragraph<'a>),
}

const CONTEXT_RADIUS: usize = 3;

impl<'a> StrReplace<'a> {
    pub fn new(tool_use: &ToolUse) -> Self {
        Self {
            state: State::new(Inner {
                tool_use: tool_use.to_owned(),
                edit: None,
            }),
            widget: StrReplaceWidget::Paragraph(Paragraph::new("")),
        }
    }

    pub fn update_text_edit(&mut self, edit: TextEdit) {
        let diff = edit.text_diff();

        let mut buf = vec![];
        if let Some(hunk) = diff
            .unified_diff()
            .context_radius(CONTEXT_RADIUS)
            .iter_hunks()
            .next()
        {
            if let Err(e) = hunk.to_writer(&mut buf) {
                warn!(error = ?e, "failed to write unified diff into memory");
            }
        } else {
            warn!("diff should have at least one hunk");
        }

        let diff_text = String::from_utf8_lossy(&buf).to_string();

        // Use CodeHighlight for diff highlighting
        let widget = match CodeHighlight::try_new(&diff_text, code_highlight::Lang::Diff) {
            Ok(highlight) => StrReplaceWidget::CodeHighlight(highlight),
            Err(_) => StrReplaceWidget::Paragraph(Paragraph::new(diff_text)),
        };

        let mut state = self.state.write();
        state.edit = Some(edit);
        self.widget = widget;
    }
}

impl Content for StrReplace<'_> {
    fn height(&self, width: u16) -> usize {
        match &self.widget {
            StrReplaceWidget::CodeHighlight(highlight) => highlight.height(width),
            StrReplaceWidget::Paragraph(paragraph) => paragraph.line_count(width),
        }
    }

    fn is_actionable(&self) -> bool {
        self.state.edit.is_some()
    }

    fn block_with_shortcuts_desc<'b>(&self, block: Block<'b>) -> Block<'b> {
        if self.is_actionable() {
            block
                .title_bottom(shortcuts_desc(&[("Apply", "CR")]))
                .title_bottom(shortcuts_desc(&[("Reject", "Esc")]))
        } else {
            block
        }
    }
}

impl Persistable for StrReplace<'static> {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: Inner = session::load(session)?;
        let mut inst = Self::new(&state.tool_use);
        inst.update_text_edit(state.edit.whatever_context("text edit should exist")?);
        Ok(inst)
    }
}

impl Component for StrReplace<'static> {
    fn handle_key_event(&mut self, key: &KeyEvent) {
        if self.state.edit.is_none() {
            return;
        }

        let is_rejecting = match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Enter) => false,
            (KeyModifiers::NONE, KeyCode::Esc) => true,
            _ => {
                return;
            } // ignore
        };

        let Some(edit) = self.state.write().edit.take() else {
            return;
        };

        global::action_tx()
            .send(Action::Tool(ToolAction::ApplyTextEdit {
                id: self.state.tool_use.id.clone(),
                name: STR_REPLACE_TOOL_NAME.to_string(),
                edit,
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
            }
            Event::Answer(AnswerEvent::ToolResult {
                is_error, output, ..
            }) => {
                self.widget = match output {
                    Final::Message(message) => {
                        let message_text = Text::from(message.to_owned());
                        StrReplaceWidget::Paragraph(Paragraph::new(if *is_error {
                            message_text.red()
                        } else {
                            message_text.green()
                        }))
                    }
                    _ => {
                        warn!(?event, "StrReplace tool should only return Final::Message");
                        StrReplaceWidget::Paragraph(Paragraph::new(""))
                    }
                };
            }
            _ => {
                handle_component_event!(self, event);
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        match &mut self.widget {
            StrReplaceWidget::CodeHighlight(highlight) => {
                highlight.draw(frame, area)?;
            }
            StrReplaceWidget::Paragraph(paragraph) => {
                frame.render_widget(&*paragraph, area);
            }
        }
        Ok(())
    }
}

impl ContentComponent for StrReplace<'static> {}
