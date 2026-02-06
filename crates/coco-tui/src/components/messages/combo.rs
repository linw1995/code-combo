use std::ops::Range;

use coco_macro::{ComponentExt, ContentComponentExt};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout},
    prelude::Rect,
    style::Style,
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use coco_highlight::Lang;
use code_combo::{
    OutputChunk, StreamKind, ToolUse,
    tools::{BashInput, Final, RunComboOutput},
};

use super::RoleMergeMode;
use super::fold::FoldState;
use super::streaming::StreamedLines;
use crate::{
    actions::{Action, ToolAction},
    components::{
        CodeHighlight, Component, Content, ContentComponent, Message, Messages, NavigationKey,
        NavigationResult, Persistable, Plain, ShortcutHints, Thinking, Tool,
    },
    error::*,
    events::{AnswerEvent, AskEvent, ComboEvent, Event},
    global::{self, State},
    session::{self, Session},
    theme::FinalizedTheme,
    widgets::Paragraph,
};

mod prompt_reply;
use prompt_reply::PromptReply;

#[derive(Serialize, Deserialize, Default)]
enum StarterState {
    #[default]
    Discovering,
    /// Awaiting user permission for Bash Tool execution (Method 2)
    AwaitingPermission,
    NotFound,
    Cancelled,
    Executing,
    Finalized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum ComboView {
    Stdout,
    Stderr,
    Mixed,
    #[default]
    Messages,
}

#[derive(Serialize, Deserialize)]
struct Inner {
    tool_use_id: String,
    name: String,
    #[serde(default)]
    command_line: String,
    is_error: bool,
    starter_state: StarterState,
    #[serde(default = "default_display_state")]
    display_state: FoldState,
    #[serde(default)]
    view: ComboView,
    #[serde(default)]
    output_chunks: Vec<OutputChunk>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    /// Bash ToolUse for permission handling (Method 2: LLM calls `coco combo run` via Bash)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bash_tool_use: Option<ToolUse>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            tool_use_id: String::new(),
            name: String::new(),
            command_line: String::new(),
            is_error: false,
            starter_state: StarterState::default(),
            display_state: default_display_state(),
            view: ComboView::default(),
            output_chunks: Vec::new(),
            summary: None,
            bash_tool_use: None,
        }
    }
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "combo")]
pub struct Combo {
    state: State<Inner>,
    command: Option<CodeHighlight<'static>>,
    messages: Messages,
    preview_lines: StreamedLines,
    is_focused: bool,
    is_child_focused: bool,
    has_child_output: bool,
    is_recording: bool,
    combo_stream_suppressed: bool,
    summary_streaming: bool,
    summary_stream_has_output: bool,
}

const LIMIT: usize = 10;
const STREAM_MARKER: &str = "▐";
const COMMAND_PROMPT: &str = "$";
const COMMAND_SPACING: u16 = 1;
const COMMAND_PADDING: u16 = 1;

fn default_display_state() -> FoldState {
    FoldState::Collapsed
}

fn default_preview_lines() -> StreamedLines {
    StreamedLines::new(Some(LIMIT))
}

impl Combo {
    pub fn new(tool_use_id: &str, name: &str) -> Self {
        Self {
            state: State::new(Inner {
                tool_use_id: tool_use_id.to_string(),
                name: name.to_string(),
                starter_state: StarterState::Executing,
                ..Default::default()
            }),
            command: None,
            messages: Messages::default()
                .with_role_merge_mode(RoleMergeMode::MergeSkipFirstUser)
                .with_compact_role(true),
            preview_lines: default_preview_lines(),
            is_focused: false,
            is_child_focused: false,
            has_child_output: false,
            is_recording: false,
            combo_stream_suppressed: false,
            summary_streaming: false,
            summary_stream_has_output: false,
        }
    }

    /// Create a Combo for Method 2: LLM calls `coco combo run` via Bash Tool.
    /// The Combo starts in Discovering state and will transition to AwaitingPermission
    /// when it receives AskEvent::ToolUsePermission.
    pub fn new_with_bash_tool_use(tool_use: ToolUse, name: &str) -> Self {
        // Extract command from Bash ToolUse input
        let command_line = serde_json::from_value::<BashInput>(tool_use.input.clone())
            .map(|input| input.command)
            .unwrap_or_default();
        let command = Self::build_command_highlight(&command_line);

        Self {
            state: State::new(Inner {
                tool_use_id: tool_use.id.clone(),
                name: name.to_string(),
                command_line,
                starter_state: StarterState::Discovering,
                bash_tool_use: Some(tool_use),
                ..Default::default()
            }),
            command,
            messages: Messages::default()
                .with_role_merge_mode(RoleMergeMode::MergeSkipFirstUser)
                .with_compact_role(true),
            preview_lines: default_preview_lines(),
            is_focused: false,
            is_child_focused: false,
            has_child_output: false,
            is_recording: false,
            combo_stream_suppressed: false,
            summary_streaming: false,
            summary_stream_has_output: false,
        }
    }

    pub(crate) fn matches_id(&self, id: &str) -> bool {
        self.state.tool_use_id == id
    }

    pub fn is_pending_permission(&self) -> bool {
        self.requiring_permission()
    }

    fn requiring_permission(&self) -> bool {
        matches!(self.state.starter_state, StarterState::AwaitingPermission)
    }

    fn has_collapsible_body(&self) -> bool {
        matches!(self.state.starter_state, StarterState::Finalized) && self.has_body_content()
    }

    fn can_focus_messages(&self) -> bool {
        if !matches!(self.state.view, ComboView::Messages) {
            return false;
        }
        if self.has_collapsible_body() && self.state.display_state.is_collapsed() {
            return false;
        }
        self.messages.has_actionable()
    }

    fn clear_child_focus(&mut self) {
        self.is_child_focused = false;
        self.messages.blur();
    }

    fn handle_enter_key(&mut self) {
        if !self.can_focus_messages() || self.is_child_focused {
            return;
        }
        if self.messages.select_first_actionable() {
            self.is_child_focused = true;
        }
    }

    fn clear_combo_stream(&mut self) {
        self.preview_lines = StreamedLines::new(Some(LIMIT));
    }

    fn update_command_line(&mut self, command_line: String) {
        self.state.write().command_line = command_line.clone();
        self.command = Self::build_command_highlight(&command_line);
    }

    fn command_height(&self, width: u16) -> u16 {
        let Some(command) = self.command.as_ref() else {
            return 0;
        };
        let width = width.max(1);
        let prompt_width = COMMAND_PROMPT.len().max(1) as u16;
        let spacing_width = COMMAND_SPACING;
        if width <= prompt_width + spacing_width {
            return 1 + COMMAND_PADDING.saturating_mul(2);
        }
        let content_width = width.saturating_sub(prompt_width + spacing_width).max(1);
        let command_height = u16::try_from(command.height(content_width)).unwrap_or(u16::MAX);
        command_height
            .max(1)
            .saturating_add(COMMAND_PADDING.saturating_mul(2))
    }

    fn build_command_highlight(command_line: &str) -> Option<CodeHighlight<'static>> {
        if command_line.trim().is_empty() {
            return None;
        }
        Some(CodeHighlight::try_new(command_line, Lang::Bash).expect("failed to new CodeHighlight"))
    }

    fn push_record_tool_use(&mut self, tool_use: ToolUse) {
        self.state.write().view = ComboView::Messages;
        let executing = tool_use.clone();
        self.messages
            .push(Message::user(Tool::new(tool_use).into()));
        self.messages
            .apply_action_to_last(&Action::Tool(ToolAction::GrantSession(executing)));
        self.has_child_output = false;
    }

    fn push_prompt(&mut self, prompt: &str) {
        self.state.write().view = ComboView::Messages;
        self.messages
            .push(Message::user(Plain::new(prompt.to_string()).into()));
    }

    fn push_prompt_reply(&mut self, tool_use: &ToolUse) {
        self.state.write().view = ComboView::Messages;
        let params = PromptReply::new(tool_use);
        self.messages.push(Message::bot(params.into()));
    }

    fn push_prompt_thinking(&mut self, thinking: &str) {
        self.state.write().view = ComboView::Messages;
        self.messages
            .push(Message::bot(Thinking::new(thinking.to_string()).into()));
    }

    fn push_summary(&mut self, summary: &str) {
        self.state.write().view = ComboView::Messages;
        self.messages
            .push(Message::bot(Plain::new(summary.to_string()).into()));
    }

    pub(crate) fn append_summary(&mut self, summary: &str, thinking: &[String]) -> bool {
        if self.summary_streaming {
            return false;
        }
        if self.state.summary.is_some() {
            return false;
        }
        let trimmed = summary.trim();
        if trimmed.is_empty() {
            return false;
        }
        for block in thinking {
            let block = block.trim();
            if block.is_empty() {
                continue;
            }
            self.messages
                .push(Message::bot(Thinking::new(block.to_string()).into()));
        }
        self.state.write().summary = Some(trimmed.to_string());
        self.push_summary(trimmed);
        true
    }

    fn push_offload_bash_tool_use(&mut self, tool_use: ToolUse, requires_confirmation: bool) {
        {
            let mut state = self.state.write();
            state.view = ComboView::Messages;
            state.bash_tool_use = if requires_confirmation {
                Some(tool_use.clone())
            } else {
                None
            };
        }
        let tool_use_id = tool_use.id.clone();
        self.messages.push(Message::bot(Tool::new(tool_use).into()));
        if requires_confirmation {
            self.state.write().starter_state = StarterState::AwaitingPermission;
            let ask = Event::Ask(AskEvent::ToolUsePermission(tool_use_id));
            let _ = self.messages.on_tool_event(&ask);
            if self.messages.select_first_actionable() {
                self.is_child_focused = true;
            }
        }
    }

    fn forward_output_to_child(&mut self, tool_use_id: &str, chunk: &OutputChunk) -> bool {
        let event = Event::Answer(AnswerEvent::ToolOutput {
            id: tool_use_id.to_string(),
            chunk: chunk.clone(),
        });
        self.messages.on_tool_event(&event).is_some()
    }

    fn forward_result_to_child(
        &mut self,
        tool_use_id: &str,
        is_error: bool,
        output: Final,
    ) -> bool {
        let event = Event::Answer(AnswerEvent::ToolResult {
            id: tool_use_id.to_string(),
            is_error,
            is_user_cancelled: false,
            output,
        });
        self.messages.on_tool_event(&event).is_some()
    }

    fn draw_command(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let Some(command) = self.command.as_mut() else {
            return Ok(());
        };
        if area.height == 0 {
            return Ok(());
        }
        if area.height <= COMMAND_PADDING.saturating_mul(2) {
            return Ok(());
        }
        let theme = global::theme();
        let prompt_width = COMMAND_PROMPT.len().max(1) as u16;
        let spacing_width = COMMAND_SPACING;
        let [area_pad_top, area_body, area_pad_bottom] = Layout::vertical([
            Constraint::Length(COMMAND_PADDING),
            Constraint::Min(1),
            Constraint::Length(COMMAND_PADDING),
        ])
        .areas(area);
        frame.render_widget(Paragraph::new(vec![Line::from("")]), area_pad_top);
        frame.render_widget(Paragraph::new(vec![Line::from("")]), area_pad_bottom);

        if area_body.width <= prompt_width + spacing_width {
            frame.render_widget(
                Paragraph::new(vec![Line::from(Span::styled(
                    COMMAND_PROMPT,
                    theme.ui.tool_label,
                ))]),
                area_body,
            );
            return Ok(());
        }
        let [area_prompt, area_gap, area_text] = Layout::horizontal([
            Constraint::Length(prompt_width),
            Constraint::Length(spacing_width),
            Constraint::Min(1),
        ])
        .areas(area_body);
        frame.render_widget(
            Paragraph::new(vec![Line::from(Span::styled(
                COMMAND_PROMPT,
                theme.ui.tool_label,
            ))]),
            area_prompt,
        );
        command.draw(frame, area_text)?;
        frame.render_widget(Paragraph::new(vec![Line::from(" ")]), area_gap);
        Ok(())
    }

    fn on_combo_event(&mut self, event: &ComboEvent) {
        match event {
            ComboEvent::NotFound { id, .. } => {
                if self.matches_id(id) {
                    self.state.write().starter_state = StarterState::NotFound
                }
            }
            ComboEvent::Output { id, chunk, .. } => self.on_ouput_event(id, chunk, None),
            ComboEvent::RecordStart { id, tool_use, .. } => {
                if self.matches_id(id) {
                    self.is_recording = true;
                    self.combo_stream_suppressed = true;
                    self.clear_combo_stream();
                    self.push_record_tool_use(tool_use.clone());
                    self.has_child_output = false;
                }
            }
            ComboEvent::RecordOutput {
                id,
                tool_use_id,
                chunk,
                ..
            } => self.on_ouput_event(id, chunk, Some(tool_use_id.as_str())),
            ComboEvent::RecordEnd {
                id,
                tool_use_id,
                is_error,
                output,
                ..
            } => {
                if self.matches_id(id) {
                    self.forward_result_to_child(tool_use_id, *is_error, output.clone());
                    self.has_child_output = false;
                    self.is_recording = false;
                    self.combo_stream_suppressed = true;
                    self.clear_combo_stream();
                }
            }
            ComboEvent::Prompt { id, prompt, .. } => {
                if self.matches_id(id) {
                    self.messages.finalize_stream();
                    self.messages.reset_stream();
                    self.push_prompt(prompt);
                }
            }
            ComboEvent::PromptStreamReset { id, .. } => {
                if self.matches_id(id) {
                    self.messages.finalize_stream();
                    self.messages.reset_stream();
                }
            }
            ComboEvent::PromptStream {
                id,
                index,
                kind,
                text,
                ..
            } => {
                if self.matches_id(id) {
                    self.messages
                        .append_stream_text(*index, *kind, text.clone());
                }
            }
            ComboEvent::SummaryStreamReset { id, .. } => {
                if self.matches_id(id) {
                    self.state.write().view = ComboView::Messages;
                    self.messages.finalize_stream();
                    self.messages.reset_stream();
                    self.summary_streaming = true;
                    self.summary_stream_has_output = false;
                }
            }
            ComboEvent::SummaryStream {
                id,
                index,
                kind,
                text,
                ..
            } => {
                if self.matches_id(id) {
                    self.state.write().view = ComboView::Messages;
                    if !text.is_empty() {
                        self.summary_stream_has_output = true;
                    }
                    self.summary_streaming = true;
                    self.messages
                        .append_stream_text(*index, *kind, text.clone());
                }
            }
            ComboEvent::SummaryDone {
                id,
                summary,
                thinking,
                ..
            } => {
                if self.matches_id(id) {
                    self.messages.finalize_stream();
                    self.messages.reset_stream();
                    let streamed = self.summary_streaming && self.summary_stream_has_output;
                    self.summary_streaming = false;
                    self.summary_stream_has_output = false;
                    if streamed {
                        let trimmed = summary.trim();
                        if !trimmed.is_empty() && self.state.summary.is_none() {
                            self.state.write().summary = Some(trimmed.to_string());
                        }
                    } else {
                        self.append_summary(summary, thinking);
                    }
                }
            }
            ComboEvent::ReplyToolUse {
                id,
                tool_use,
                thinking,
                offload,
                requires_confirmation,
                ..
            } => {
                if self.matches_id(id) {
                    self.messages.finalize_stream();
                    self.messages.reset_stream();
                    for block in thinking {
                        self.push_prompt_thinking(block);
                    }
                    if *offload {
                        self.push_offload_bash_tool_use(tool_use.clone(), *requires_confirmation);
                    } else {
                        self.push_prompt_reply(tool_use);
                    }
                }
            }
            ComboEvent::ReplyToolResult {
                id,
                tool_use_id,
                is_error,
                output,
                ..
            } => {
                if self.matches_id(id) {
                    self.forward_result_to_child(tool_use_id, *is_error, output.clone());
                    self.state.write().bash_tool_use = None;
                }
            }
            ComboEvent::Executing {
                id, command_line, ..
            } => {
                if self.matches_id(id) {
                    self.clear_child_focus();
                    self.has_child_output = false;
                    self.is_recording = false;
                    self.combo_stream_suppressed = false;
                    let mut state = self.state.write();
                    state.starter_state = StarterState::Executing;
                    state.is_error = false;
                    state.view = ComboView::Messages;
                    state.output_chunks.clear();
                    state.summary = None;
                    state.display_state.expand();
                    drop(state);
                    self.update_command_line(command_line.clone());
                    self.preview_lines = StreamedLines::new(Some(LIMIT));
                    self.messages.reset_stream();
                    self.messages.clear();
                    self.summary_streaming = false;
                    self.summary_stream_has_output = false;
                }
            }
            ComboEvent::Executed {
                id,
                starter,
                exit_code,
                ..
            } => {
                if self.matches_id(id) {
                    let mut state = self.state.write();
                    let error_message = match (&starter.combo, *exit_code) {
                        (Err(err), _) => {
                            state.is_error = true;
                            Some(format!("Failed to execute starter: {err}"))
                        }
                        (Ok(_), Some(code)) if code != 0 => {
                            state.is_error = true;
                            Some(format!("Combo exited with status {code}"))
                        }
                        _ => {
                            state.is_error = false;
                            None
                        }
                    };
                    state.starter_state = StarterState::Finalized;
                    state.bash_tool_use = None;
                    state.display_state.expand();
                    drop(state);
                    if let Some(message) = error_message {
                        self.messages.push(
                            Message::system(Plain::new(message).into()).with_role_prefix(false),
                        );
                    }
                    self.has_child_output = false;
                }
            }
            ComboEvent::Cancelled { id, .. } => {
                if id.as_ref().map(|id| self.matches_id(id)).unwrap_or(true) {
                    self.clear_child_focus();
                    self.has_child_output = false;
                    self.is_recording = false;
                    self.combo_stream_suppressed = true;
                    self.clear_combo_stream();
                    self.messages.finalize_stream();
                    self.messages.reset_stream();
                    self.summary_streaming = false;
                    self.summary_stream_has_output = false;
                    {
                        let mut state = self.state.write();
                        state.starter_state = StarterState::Cancelled;
                        state.bash_tool_use = None;
                        state.display_state.expand();
                    }
                }
            }
            ComboEvent::ReplyToolError { .. } => {
                self.messages.finalize_stream();
                self.messages.reset_stream();
            }
            _ => (),
        }
    }

    fn on_ouput_event(&mut self, id: &str, chunk: &OutputChunk, tool_use_id: Option<&str>) {
        if !self.matches_id(id) {
            return;
        }
        if let Some(tool_use_id) = tool_use_id
            && self.forward_output_to_child(tool_use_id, chunk)
        {
            if !self.has_child_output {
                self.clear_combo_stream();
            }
            self.has_child_output = true;
            return;
        }
        {
            let mut state = self.state.write();
            state.output_chunks.push(chunk.clone());
        }
        if self.is_recording {
            return;
        }
        if self.combo_stream_suppressed {
            self.clear_combo_stream();
            self.combo_stream_suppressed = false;
        }
        self.has_child_output = false;
        self.preview_lines.push_chunk(chunk);
    }

    fn has_message_content(&self) -> bool {
        !self.messages.is_empty()
    }

    fn has_command_content(&self) -> bool {
        self.command.is_some()
    }

    fn has_body_content(&self) -> bool {
        self.has_command_content() || self.has_message_content() || self.has_stream_content()
    }

    fn has_stream_content(&self) -> bool {
        !self.combo_stream_suppressed
            && !self.has_child_output
            && (!self.state.output_chunks.is_empty() || !self.preview_lines.is_empty())
    }

    fn can_toggle_view(&self) -> bool {
        self.has_stream_content()
    }

    fn set_view(&mut self, view: ComboView) -> bool {
        if !matches!(view, ComboView::Messages) && !self.can_toggle_view() {
            return false;
        }
        if self.state.view != view {
            self.clear_child_focus();
            self.state.write().view = view;
            return true;
        }
        false
    }

    fn toggle_display_state(&mut self) {
        self.clear_child_focus();
        let mut state = self.state.write();
        state.display_state = state.display_state.toggle();
    }

    fn on_blur(&mut self) {
        self.clear_child_focus();
        if self.has_collapsible_body() {
            self.state.write().display_state.collapse();
        }
    }

    fn messages_body_height(&self, width: u16) -> u16 {
        let command_height = self.command_height(width);
        let messages_height =
            u16::try_from(self.messages.height_for_width(width)).unwrap_or(u16::MAX);
        let preview_height = if matches!(self.state.starter_state, StarterState::Executing)
            && !self.has_child_output
            && !self.combo_stream_suppressed
        {
            self.preview_lines.len() as u16
        } else {
            0
        };
        let spacing = if messages_height > 0 && preview_height > 0 {
            1
        } else {
            0
        };
        command_height
            .saturating_add(messages_height)
            .saturating_add(spacing)
            .saturating_add(preview_height)
    }

    fn stream_line_count(&self, width: u16, view: ComboView) -> u16 {
        if matches!(view, ComboView::Messages) {
            return 0;
        }
        let command_height = self.command_height(width);
        if self.has_child_output || self.combo_stream_suppressed {
            return command_height;
        }
        let total = self
            .state
            .output_chunks
            .iter()
            .filter(|chunk| Self::matches_stream_view(view, chunk.stream))
            .map(|chunk| chunk.lines.len())
            .sum::<usize>();
        command_height.saturating_add(u16::try_from(total).unwrap_or(u16::MAX))
    }

    fn matches_stream_view(view: ComboView, stream: StreamKind) -> bool {
        match view {
            ComboView::Stdout => matches!(stream, StreamKind::Stdout),
            ComboView::Stderr => matches!(stream, StreamKind::Stderr),
            ComboView::Mixed => true,
            ComboView::Messages => false,
        }
    }

    fn draw_messages_view(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        if area.height == 0 {
            return Ok(());
        }
        let command_height = self.command_height(area.width);
        let messages_height =
            u16::try_from(self.messages.height_for_width(area.width)).unwrap_or(u16::MAX);
        let preview_height = if matches!(self.state.starter_state, StarterState::Executing)
            && !self.has_child_output
            && !self.combo_stream_suppressed
        {
            self.preview_lines.len() as u16
        } else {
            0
        };
        let spacing = if messages_height > 0 && preview_height > 0 {
            1
        } else {
            0
        };

        let mut rows = Vec::new();
        if command_height > 0 {
            rows.push(Constraint::Length(command_height));
        }
        if messages_height > 0 {
            rows.push(Constraint::Length(messages_height));
        }
        if spacing > 0 {
            rows.push(Constraint::Length(spacing));
        }
        if preview_height > 0 {
            rows.push(Constraint::Length(preview_height));
        }

        if rows.is_empty() {
            return Ok(());
        }

        let chunks = Layout::vertical(rows).split(area);
        let mut idx = 0;
        if command_height > 0 {
            self.draw_command(frame, chunks[idx])?;
            idx += 1;
        }
        if messages_height > 0 {
            self.messages.draw_inline(frame, chunks[idx])?;
            idx += 1;
        }
        if spacing > 0 {
            idx += 1;
        }
        if preview_height > 0 {
            let (output, markers) = self.render_preview_lines();
            self.draw_stream_section(frame, chunks[idx], &output, markers.as_ref());
        }
        Ok(())
    }

    fn draw_stream_view(&mut self, frame: &mut Frame, area: Rect, view: ComboView) -> Result<()> {
        if area.height == 0 {
            return Ok(());
        }
        let (output, markers) = self.render_stream_lines(view);
        let command_height = self.command_height(area.width);
        if command_height == 0 {
            self.draw_stream_section(frame, area, &output, markers.as_ref());
            return Ok(());
        }

        use Constraint::Length;
        let remaining_height = area.height.saturating_sub(command_height);
        let [area_command, area_output] =
            Layout::vertical([Length(command_height), Length(remaining_height)]).areas(area);
        self.draw_command(frame, area_command)?;
        self.draw_stream_section(frame, area_output, &output, markers.as_ref());
        Ok(())
    }

    fn draw_stream_section(
        &self,
        frame: &mut Frame,
        area: Rect,
        output: &Paragraph<'static>,
        markers: Option<&Paragraph<'static>>,
    ) {
        let marker_width = if markers.is_some() && area.width > 1 {
            1
        } else {
            0
        };
        if marker_width == 0 {
            frame.render_widget(output, area);
            return;
        }
        let [area_text, area_markers] =
            Layout::horizontal([Constraint::Min(1), Constraint::Length(marker_width)]).areas(area);
        frame.render_widget(output, area_text);
        if let Some(markers) = markers {
            frame.render_widget(markers, area_markers);
        }
    }

    fn render_preview_lines(&self) -> (Paragraph<'static>, Option<Paragraph<'static>>) {
        let theme = global::theme();
        let mut lines = Vec::new();
        let mut markers = Vec::new();
        for line in self.preview_lines.iter() {
            let marker_style = match line.stream {
                StreamKind::Stdout => theme.ui.bash_stdout_marker,
                StreamKind::Stderr => theme.ui.bash_stderr_marker,
            };
            lines.push(Line::from(line.text.clone()));
            markers.push(Line::from(Span::styled(STREAM_MARKER, marker_style)));
        }
        let output = Paragraph::new(lines);
        let markers = if markers.is_empty() {
            None
        } else {
            Some(Paragraph::new(markers))
        };
        (output, markers)
    }

    fn render_stream_lines(
        &self,
        view: ComboView,
    ) -> (Paragraph<'static>, Option<Paragraph<'static>>) {
        let theme = global::theme();
        let mut lines = Vec::new();
        let mut markers = Vec::new();
        let show_markers = matches!(view, ComboView::Mixed);
        for chunk in &self.state.output_chunks {
            if !Self::matches_stream_view(view, chunk.stream) {
                continue;
            }
            let marker_style = match chunk.stream {
                StreamKind::Stdout => theme.ui.bash_stdout_marker,
                StreamKind::Stderr => theme.ui.bash_stderr_marker,
            };
            for line in &chunk.lines {
                lines.push(Line::from(line.clone()));
                if show_markers {
                    markers.push(Line::from(Span::styled(STREAM_MARKER, marker_style)));
                }
            }
        }
        let output = Paragraph::new(lines);
        let markers = if !show_markers || markers.is_empty() {
            None
        } else {
            Some(Paragraph::new(markers))
        };
        (output, markers)
    }

    fn get_title_spans(&self, theme: &FinalizedTheme) -> Vec<Span<'_>> {
        let apply_dim = |style: Style| {
            if self.is_focused {
                style
            } else {
                style.patch(theme.ui.combo_title_dim)
            }
        };

        let (state_text, state_style) = match self.state.starter_state {
            StarterState::Discovering => (
                " Discovering combo starters...",
                theme.ui.combo_title_state_discovering,
            ),
            StarterState::AwaitingPermission => (
                " Awaiting confirmation",
                theme.ui.tool_title_state_pending_confirmation,
            ),
            StarterState::NotFound => (" Not found", theme.ui.combo_title_state_not_found),
            StarterState::Cancelled => (" Cancelled", theme.ui.combo_title_state_cancelled),
            StarterState::Executing => (" Executing...", theme.ui.combo_title_state_executing),
            StarterState::Finalized => {
                if self.state.is_error {
                    (" Failed", theme.ui.combo_title_state_failed)
                } else {
                    (" Completed", theme.ui.combo_title_state_completed)
                }
            }
        };

        let mut spans = vec![Span::styled(
            " Combo:",
            apply_dim(theme.ui.combo_title_name),
        )];

        match self.state.starter_state {
            StarterState::Discovering => {
                spans.push(Span::styled(state_text, apply_dim(state_style)));
            }
            _ => {
                spans.push(Span::styled(
                    format!(" {}", self.state.name),
                    apply_dim(theme.ui.combo_title_name),
                ));
                spans.push(Span::styled(state_text, apply_dim(state_style)));
            }
        }

        if let Some(line) = self.reminder_line() {
            spans.extend(line.spans.into_iter().map(|mut span| {
                span.style = apply_dim(span.style);
                span
            }));
        }
        spans.push(Span::raw(" "));
        spans
    }

    fn maybe_append_summary(&mut self, output: &Final, is_error: bool) {
        if self.state.summary.is_some() {
            return;
        }
        let Some(summary) = combo_summary_from_output(output, is_error) else {
            return;
        };
        self.append_summary(&summary.summary, &summary.thinking);
    }
}

impl Content for Combo {
    fn height(&self, width: u16) -> usize {
        let border_height: usize = 1;
        if self.has_collapsible_body() && self.state.display_state.is_collapsed() {
            let command_height = self.command_height(width) as usize;
            return border_height.saturating_add(command_height);
        }
        let body_height = match self.state.view {
            ComboView::Messages => self.messages_body_height(width),
            view => self.stream_line_count(width, view),
        };
        body_height as usize + border_height
    }

    fn is_actionable(&self) -> bool {
        self.requiring_permission()
            || self.has_collapsible_body()
            || self.can_toggle_view()
            || self.can_focus_messages()
    }

    fn focus_range(&self, width: u16) -> Option<Range<u16>> {
        if !self.is_child_focused {
            return None;
        }
        if !matches!(self.state.view, ComboView::Messages) {
            return None;
        }
        if self.has_collapsible_body() && self.state.display_state.is_collapsed() {
            return None;
        }
        let border_height: u16 = 1;
        let command_height = self.command_height(width);
        let range = self.messages.focus_range(width, 0)?;
        let offset = border_height.saturating_add(command_height);
        let start = range.start.saturating_add(offset);
        let end = range.end.saturating_add(offset);
        Some(start..end)
    }

    fn shortcut_hints(&self) -> ShortcutHints {
        // Show permission hints when awaiting confirmation
        if self.requiring_permission() {
            let mut hints = ShortcutHints::default();
            hints.push_visible(&[("Run", "CR"), ("Allow in Session", "A")]);
            hints.push_visible(&[("Cancel", "Esc")]);
            return hints;
        }

        if !self.has_collapsible_body() && !self.can_toggle_view() && !self.can_focus_messages() {
            return ShortcutHints::default();
        }
        let mut hints = ShortcutHints::default();
        if self.is_child_focused {
            hints.extend(self.messages.shortcut_hints());
            if self.messages.has_thinking_toggle_for_focus() {
                hints.push_visible(&[("Thinking", "r")]);
            }
        }
        if self.can_focus_messages() {
            if self.is_child_focused {
                hints.push_visible(&[("Back", "Esc")]);
            } else {
                hints.push_visible(&[("Enter", "CR")]);
            }
        }
        if self.has_collapsible_body() && !self.is_child_focused {
            let toggle_text = if self.state.display_state.is_collapsed() {
                ("Unfold", "z")
            } else {
                ("Fold", "z")
            };
            hints.push_visible(&[toggle_text]);
        }
        if self.can_toggle_view() && !self.is_child_focused {
            hints.push_visible(&[("View", "1/2/3/4")]);
        }
        hints
    }

    fn reminder_line(&self) -> Option<Line<'static>> {
        if self.has_collapsible_body() && self.state.display_state.is_collapsed() {
            let theme = global::theme();
            Some(Line::from(Span::styled(" (folded)", theme.ui.folded_hint)))
        } else {
            None
        }
    }
}

impl Persistable for Combo {
    fn save(&self) -> Session {
        session::save_related(&self.state, self.messages.save())
    }

    fn load(session: Session) -> Result<Self> {
        let (state, messages): (Inner, Session) = session::load_related(session)?;
        let command = Self::build_command_highlight(&state.command_line);
        let preview_lines = StreamedLines::from_chunks(&state.output_chunks, Some(LIMIT));
        let mut loaded_messages = Messages::load(messages)?;
        loaded_messages = loaded_messages
            .with_role_merge_mode(RoleMergeMode::MergeSkipFirstUser)
            .with_compact_role(true);
        let combo = Self {
            state: State::new(state),
            command,
            messages: loaded_messages,
            preview_lines,
            is_focused: false,
            is_child_focused: false,
            has_child_output: false,
            is_recording: false,
            combo_stream_suppressed: false,
            summary_streaming: false,
            summary_stream_has_output: false,
        };
        Ok(combo)
    }
}

impl Component for Combo {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        let mut children: Vec<&mut dyn Component> = Vec::with_capacity(2);
        if let Some(command) = self.command.as_mut() {
            children.push(command);
        }
        children.push(&mut self.messages);
        Box::new(children.into_iter())
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        // Handle permission actions when awaiting confirmation
        if self.requiring_permission() {
            if let Some(tool_use) = self.state.bash_tool_use.clone() {
                match (key.modifiers, key.code) {
                    (KeyModifiers::NONE, KeyCode::Enter) => {
                        self.state.write().starter_state = StarterState::Discovering;
                        global::action_tx()
                            .send(ToolAction::Grant(tool_use).into())
                            .unwrap();
                    }
                    (KeyModifiers::NONE, KeyCode::Char('a') | KeyCode::Char('A')) => {
                        self.state.write().starter_state = StarterState::Discovering;
                        global::action_tx()
                            .send(ToolAction::GrantSession(tool_use).into())
                            .unwrap();
                    }
                    (KeyModifiers::NONE, KeyCode::Esc) => {
                        self.state.write().starter_state = StarterState::Cancelled;
                        global::action_tx()
                            .send(ToolAction::Cancel(tool_use).into())
                            .unwrap();
                    }
                    _ => {}
                }
            }
            return;
        }

        if self.is_child_focused {
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Esc) => self.clear_child_focus(),
                (KeyModifiers::NONE, KeyCode::Char('r')) => {
                    self.messages.toggle_thinking_for_focus();
                }
                _ => self.messages.handle_key_event(key),
            }
            return;
        }

        if let (KeyModifiers::NONE, KeyCode::Enter) = (key.modifiers, key.code) {
            self.handle_enter_key();
            return;
        };

        if !self.has_collapsible_body() && !self.can_toggle_view() && !self.can_focus_messages() {
            return;
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('1')) => {
                self.set_view(ComboView::Stdout);
            }
            (KeyModifiers::NONE, KeyCode::Char('2')) => {
                self.set_view(ComboView::Stderr);
            }
            (KeyModifiers::NONE, KeyCode::Char('3')) => {
                self.set_view(ComboView::Mixed);
            }
            (KeyModifiers::NONE, KeyCode::Char('4')) => {
                self.set_view(ComboView::Messages);
            }
            (KeyModifiers::NONE, KeyCode::Char('z')) if self.has_collapsible_body() => {
                self.toggle_display_state();
            }
            _ => {}
        }
    }

    fn handle_event(&mut self, event: &Event) {
        match event {
            Event::Combo(event) => {
                self.on_combo_event(event);
            }
            Event::Ask(AskEvent::ToolUsePermission(id)) => {
                // Handle permission request for Bash Tool (Method 2)
                if self.matches_id(id) && self.state.bash_tool_use.is_some() {
                    self.state.write().starter_state = StarterState::AwaitingPermission;
                }
            }
            Event::Answer(AnswerEvent::ToolResult {
                id,
                is_error,
                output,
                ..
            }) => {
                if self.matches_id(id) {
                    self.maybe_append_summary(output, *is_error);
                } else {
                    // Route tool events to the correct child component by id
                    self.messages.on_tool_event(event);
                }
            }
            Event::Answer(AnswerEvent::ToolOutput { .. }) => {
                // Route tool events to the correct child component by id
                self.messages.on_tool_event(event);
            }
            _ => {
                handle_component_event!(self, event);
            }
        }
    }

    fn update(&mut self, action: &Action) {
        match action {
            Action::Focus => {
                self.is_focused = true;
            }
            Action::Blur => {
                self.is_focused = false;
                self.on_blur();
            }
            _ => (),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
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
        frame.render_widget(&block, area);

        if self.has_collapsible_body() && self.state.display_state.is_collapsed() {
            let output_area = block.inner(area);
            self.draw_command(frame, output_area)?;
            return Ok(());
        }

        let output_area = block.inner(area);

        match self.state.view {
            ComboView::Messages => self.draw_messages_view(frame, output_area)?,
            view => self.draw_stream_view(frame, output_area, view)?,
        }
        Ok(())
    }

    fn handle_navigation(&mut self, key: NavigationKey) -> NavigationResult {
        if !self.is_child_focused {
            return NavigationResult::Ignored;
        }
        let moved = match key {
            NavigationKey::Up => self.messages.select_prev_actionable(),
            NavigationKey::Down => self.messages.select_next_actionable(),
        };
        if moved {
            NavigationResult::Moved
        } else {
            NavigationResult::Boundary
        }
    }
}

struct ComboSummary {
    summary: String,
    thinking: Vec<String>,
}

fn combo_summary_from_output(output: &Final, _is_error: bool) -> Option<ComboSummary> {
    match output {
        Final::Json(value) => {
            if let Ok(parsed) = serde_json::from_value::<RunComboOutput>(value.clone()) {
                let summary = parsed.summary.trim();
                if !summary.is_empty() {
                    return Some(ComboSummary {
                        summary: summary.to_string(),
                        thinking: parsed.summary_thinking,
                    });
                }
                if let Some(error) = parsed.error {
                    let error = error.trim();
                    if !error.is_empty() {
                        return Some(ComboSummary {
                            summary: error.to_string(),
                            thinking: parsed.summary_thinking,
                        });
                    }
                }
                return None;
            }
            let summary = value
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if !summary.is_empty() {
                return Some(ComboSummary {
                    summary: summary.to_string(),
                    thinking: Vec::new(),
                });
            }
            value
                .get("error")
                .and_then(Value::as_str)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .map(|summary| ComboSummary {
                    summary,
                    thinking: Vec::new(),
                })
        }
        Final::Message(message) => {
            let trimmed = message.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(ComboSummary {
                    summary: trimmed.to_string(),
                    thinking: Vec::new(),
                })
            }
        }
    }
}

impl ContentComponent for Combo {}

#[cfg(test)]
mod tests {
    use crate::actions::Action;
    use crate::events::{ComboEvent, Event};

    use super::*;
    use code_combo::tools::{BASH_TOOL_NAME, BashInput};

    const TEST_ID: &str = "test_combo_id";
    const TEST_NAME: &str = "demo";

    fn test_key_z() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE)
    }

    fn test_key_enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    }

    fn test_key_backspace() -> KeyEvent {
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)
    }

    fn make_starter(name: &str) -> code_combo::Starter {
        code_combo::Starter {
            path: "/tmp/demo".to_string(),
            combo: Ok(code_combo::Combo {
                metadata: code_combo::ComboMetadata {
                    name: name.to_string(),
                    description: String::new(),
                },
            }),
        }
    }

    fn make_tool_use(id: &str, command: &str) -> ToolUse {
        let input = BashInput::new(command.to_string());
        ToolUse {
            id: id.to_string(),
            name: BASH_TOOL_NAME.to_string(),
            input: serde_json::to_value(&input).unwrap(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_is_collapsed_by_default_and_toggles_with_z() {
        let mut combo = Combo::new(TEST_ID, TEST_NAME);
        combo.handle_event(&Event::Combo(ComboEvent::Prompt {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            prompt: "line1".to_string(),
            thinking: None,
        }));
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            starter: make_starter(TEST_NAME),
            exit_code: None,
        }));

        assert!(combo.height(80) > 1);
        combo.handle_action(&Action::Blur);
        assert_eq!(combo.height(80), 1);
        combo.handle_key_event(&test_key_z());
        assert!(combo.height(80) > 1);
        combo.handle_key_event(&test_key_z());
        assert_eq!(combo.height(80), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_is_visible_while_executing() {
        let mut combo = Combo::new(TEST_ID, TEST_NAME);
        combo.handle_event(&Event::Combo(ComboEvent::Executing {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            command_line: "demo".to_string(),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::Output {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            chunk: code_combo::OutputChunk {
                timestamp: 0,
                stream: code_combo::StreamKind::Stdout,
                lines: vec!["line1".to_string(), "line2".to_string()],
            },
        }));

        assert_eq!(combo.height(80), 6);
        combo.handle_key_event(&test_key_z());
        assert_eq!(combo.height(80), 6);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_persists_collapsed_state() {
        let mut combo = Combo::new(TEST_ID, TEST_NAME);
        combo.handle_event(&Event::Combo(ComboEvent::Prompt {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            prompt: "line1".to_string(),
            thinking: None,
        }));
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            starter: make_starter(TEST_NAME),
            exit_code: None,
        }));
        combo.handle_action(&Action::Blur);
        combo.handle_key_event(&test_key_z());
        assert!(combo.height(80) > 1);

        let session = combo.save();
        let loaded = Combo::load(session).unwrap();
        assert!(loaded.height(80) > 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_marks_error_on_nonzero_exit() {
        let mut combo = Combo::new(TEST_ID, TEST_NAME);
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            starter: make_starter(TEST_NAME),
            exit_code: Some(1),
        }));

        assert!(combo.state.is_error);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_enters_and_exits_actionable_messages_with_enter_and_backspace() {
        let mut combo = Combo::new(TEST_ID, TEST_NAME);
        combo.handle_event(&Event::Combo(ComboEvent::Executing {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            command_line: "demo".to_string(),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordStart {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            tool_use: make_tool_use("combo_record_demo_0", "echo 1"),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordEnd {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            tool_use_id: "combo_record_demo_0".to_string(),
            is_error: false,
            output: Final::Message("ok".to_string()),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordStart {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            tool_use: make_tool_use("combo_record_demo_1", "echo 2"),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordEnd {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            tool_use_id: "combo_record_demo_1".to_string(),
            is_error: false,
            output: Final::Message("ok".to_string()),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            starter: make_starter(TEST_NAME),
            exit_code: None,
        }));

        assert!(!combo.is_child_focused);
        assert_eq!(combo.messages.selected_idx(), None);

        combo.handle_key_event(&test_key_enter());
        assert!(combo.is_child_focused);
        assert_eq!(combo.messages.selected_idx(), Some(0));

        combo.handle_key_event(&test_key_backspace());
        assert!(!combo.is_child_focused);
        assert_eq!(combo.messages.selected_idx(), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_moves_actionable_messages_with_jk() {
        let mut combo = Combo::new(TEST_ID, TEST_NAME);
        combo.handle_event(&Event::Combo(ComboEvent::Executing {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            command_line: "demo".to_string(),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordStart {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            tool_use: make_tool_use("combo_record_demo_0", "echo 1"),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordEnd {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            tool_use_id: "combo_record_demo_0".to_string(),
            is_error: false,
            output: Final::Message("ok".to_string()),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordStart {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            tool_use: make_tool_use("combo_record_demo_1", "echo 2"),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordEnd {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            tool_use_id: "combo_record_demo_1".to_string(),
            is_error: false,
            output: Final::Message("ok".to_string()),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            starter: make_starter(TEST_NAME),
            exit_code: None,
        }));

        combo.handle_key_event(&test_key_enter());
        assert_eq!(combo.messages.selected_idx(), Some(0));

        combo.handle_navigation(NavigationKey::Down);
        assert_eq!(combo.messages.selected_idx(), Some(1));

        combo.handle_navigation(NavigationKey::Up);
        assert_eq!(combo.messages.selected_idx(), Some(0));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_routes_output_to_child_component() {
        let mut combo = Combo::new(TEST_ID, TEST_NAME);
        let tool_use = make_tool_use("combo_record_demo_0", "echo 1");
        combo.handle_event(&Event::Combo(ComboEvent::Executing {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            command_line: "demo".to_string(),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordStart {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            tool_use,
        }));

        combo.handle_event(&Event::Combo(ComboEvent::RecordOutput {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            tool_use_id: "combo_record_demo_0".to_string(),
            chunk: code_combo::OutputChunk {
                timestamp: 0,
                stream: code_combo::StreamKind::Stdout,
                lines: vec!["line1".to_string()],
            },
        }));

        assert!(combo.has_child_output);
        assert!(combo.state.output_chunks.is_empty());
        assert!(combo.preview_lines.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_suppresses_stream_until_new_output_after_record() {
        let mut combo = Combo::new(TEST_ID, TEST_NAME);
        let tool_use = make_tool_use("combo_record_demo_0", "echo 1");
        combo.handle_event(&Event::Combo(ComboEvent::Executing {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            command_line: "demo".to_string(),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordStart {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            tool_use,
        }));

        combo.handle_event(&Event::Combo(ComboEvent::Output {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            chunk: code_combo::OutputChunk {
                timestamp: 0,
                stream: code_combo::StreamKind::Stdout,
                lines: vec!["combo-line".to_string()],
            },
        }));

        assert!(combo.combo_stream_suppressed);
        assert_eq!(combo.state.output_chunks.len(), 1);
        assert!(combo.preview_lines.is_empty());

        combo.handle_event(&Event::Combo(ComboEvent::RecordEnd {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            tool_use_id: "combo_record_demo_0".to_string(),
            is_error: false,
            output: Final::Message("ok".to_string()),
        }));

        assert!(combo.combo_stream_suppressed);
        assert_eq!(combo.state.output_chunks.len(), 1);
        assert!(combo.preview_lines.is_empty());

        combo.handle_event(&Event::Combo(ComboEvent::Output {
            id: TEST_ID.to_string(),
            name: TEST_NAME.to_string(),
            chunk: code_combo::OutputChunk {
                timestamp: 0,
                stream: code_combo::StreamKind::Stdout,
                lines: vec!["after-record".to_string()],
            },
        }));

        assert!(!combo.combo_stream_suppressed);
        assert_eq!(combo.state.output_chunks.len(), 2);
        assert_eq!(combo.preview_lines.len(), 1);
    }
}
