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

use coco_highlight::Lang;
use code_combo::{OutputChunk, StreamKind, ToolUse, tools::Final};

use super::fold::FoldState;
use super::streaming::StreamedLines;
use crate::{
    actions::{Action, ToolAction},
    components::{
        CodeHighlight, Component, Content, ContentComponent, Message, Messages, NavigationKey,
        NavigationResult, Persistable, Plain, ShortcutHints, Tool,
    },
    error::*,
    events::{AnswerEvent, ComboEvent, Event},
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
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            name: String::new(),
            command_line: String::new(),
            is_error: false,
            starter_state: StarterState::default(),
            display_state: default_display_state(),
            view: ComboView::default(),
            output_chunks: Vec::new(),
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
    pub fn new(name: &str) -> Self {
        Self {
            state: State::new(Inner {
                name: name.to_string(),
                ..Default::default()
            }),
            command: None,
            messages: Messages::default(),
            preview_lines: default_preview_lines(),
            is_focused: false,
            is_child_focused: false,
            has_child_output: false,
            is_recording: false,
            combo_stream_suppressed: false,
        }
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
            .push(Message::user(Tool::new(tool_use).into()).with_role_prefix(false));
        self.messages
            .apply_action_to_last(&Action::Tool(ToolAction::GrantSession(executing)));
        self.has_child_output = false;
    }

    fn push_prompt(&mut self, prompt: &str) {
        self.state.write().view = ComboView::Messages;
        self.messages
            .push(Message::user(Plain::new(prompt.to_string()).into()).with_role_prefix(false));
    }

    fn push_prompt_reply(&mut self, tool_use: &ToolUse) {
        self.state.write().view = ComboView::Messages;
        let params = PromptReply::new(tool_use);
        self.messages
            .push(Message::bot(params.into()).with_role_prefix(false));
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
            ComboEvent::NotFound { name } => {
                if &self.state.name == name {
                    self.state.write().starter_state = StarterState::NotFound
                }
            }
            ComboEvent::Output { name, chunk } => self.on_ouput_event(name, chunk, None),
            ComboEvent::RecordStart { name, tool_use } => {
                if &self.state.name == name {
                    self.is_recording = true;
                    self.combo_stream_suppressed = true;
                    self.clear_combo_stream();
                    self.push_record_tool_use(tool_use.clone());
                    self.has_child_output = false;
                }
            }
            ComboEvent::RecordOutput {
                name,
                tool_use_id,
                chunk,
            } => self.on_ouput_event(name, chunk, Some(tool_use_id.as_str())),
            ComboEvent::RecordEnd {
                name,
                tool_use_id,
                is_error,
                output,
            } => {
                if &self.state.name == name {
                    self.forward_result_to_child(tool_use_id, *is_error, output.clone());
                    self.has_child_output = false;
                    self.is_recording = false;
                    self.combo_stream_suppressed = true;
                    self.clear_combo_stream();
                }
            }
            ComboEvent::Prompt { name, prompt } => {
                if &self.state.name == name {
                    self.push_prompt(prompt);
                }
            }
            ComboEvent::PromptReply { name, tool_use } => {
                if &self.state.name == name {
                    self.push_prompt_reply(tool_use);
                }
            }
            ComboEvent::Executing { name, command_line } => {
                if &self.state.name == name {
                    self.clear_child_focus();
                    self.has_child_output = false;
                    self.is_recording = false;
                    self.combo_stream_suppressed = false;
                    let mut state = self.state.write();
                    state.starter_state = StarterState::Executing;
                    state.is_error = false;
                    state.view = ComboView::Messages;
                    state.output_chunks.clear();
                    state.display_state.expand();
                    drop(state);
                    self.update_command_line(command_line.clone());
                    self.preview_lines = StreamedLines::new(Some(LIMIT));
                    self.messages.clear();
                }
            }
            ComboEvent::Executed {
                name,
                starter,
                exit_code,
            } => {
                if &self.state.name == name {
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
            ComboEvent::Cancelled { name } => {
                if name
                    .as_ref()
                    .map(|name| name == &self.state.name)
                    .unwrap_or(true)
                {
                    self.clear_child_focus();
                    self.has_child_output = false;
                    self.is_recording = false;
                    self.combo_stream_suppressed = true;
                    self.clear_combo_stream();
                    {
                        let mut state = self.state.write();
                        state.starter_state = StarterState::Cancelled;
                        state.display_state.expand();
                    }
                }
            }
            _ => (),
        }
    }

    fn on_ouput_event(&mut self, name: &str, chunk: &OutputChunk, tool_use_id: Option<&str>) {
        if self.state.name != name {
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
        self.has_collapsible_body() || self.can_toggle_view() || self.can_focus_messages()
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
        if !self.has_collapsible_body() && !self.can_toggle_view() && !self.can_focus_messages() {
            return ShortcutHints::default();
        }
        let mut hints = ShortcutHints::default();
        if self.is_child_focused {
            hints.extend(self.messages.shortcut_hints());
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
        let combo = Self {
            state: State::new(state),
            command,
            messages: Messages::load(messages)?,
            preview_lines,
            is_focused: false,
            is_child_focused: false,
            has_child_output: false,
            is_recording: false,
            combo_stream_suppressed: false,
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
        if self.is_child_focused {
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Esc) => self.clear_child_focus(),
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
        if let Event::Combo(event) = event {
            self.on_combo_event(event);
        } else {
            handle_component_event!(self, event);
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

impl ContentComponent for Combo {}

#[cfg(test)]
mod tests {
    use crate::actions::Action;
    use crate::events::{ComboEvent, Event};

    use super::*;
    use code_combo::tools::{BASH_TOOL_NAME, BashInput};

    fn test_key_z() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE)
    }

    fn test_key_enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    }

    fn test_key_esc() -> KeyEvent {
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
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
        let mut combo = Combo::new("demo");
        combo.handle_event(&Event::Combo(ComboEvent::Prompt {
            name: "demo".to_string(),
            prompt: "line1".to_string(),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            name: "demo".to_string(),
            starter: make_starter("demo"),
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
        let mut combo = Combo::new("demo");
        combo.handle_event(&Event::Combo(ComboEvent::Executing {
            name: "demo".to_string(),
            command_line: "demo".to_string(),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::Output {
            name: "demo".to_string(),
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
        let mut combo = Combo::new("demo");
        combo.handle_event(&Event::Combo(ComboEvent::Prompt {
            name: "demo".to_string(),
            prompt: "line1".to_string(),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            name: "demo".to_string(),
            starter: make_starter("demo"),
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
        let mut combo = Combo::new("demo");
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            name: "demo".to_string(),
            starter: make_starter("demo"),
            exit_code: Some(1),
        }));

        assert!(combo.state.is_error);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_enters_and_exits_actionable_messages_with_enter_and_esc() {
        let mut combo = Combo::new("demo");
        combo.handle_event(&Event::Combo(ComboEvent::Executing {
            name: "demo".to_string(),
            command_line: "demo".to_string(),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordStart {
            name: "demo".to_string(),
            tool_use: make_tool_use("combo_record_demo_0", "echo 1"),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordEnd {
            name: "demo".to_string(),
            tool_use_id: "combo_record_demo_0".to_string(),
            is_error: false,
            output: Final::Message("ok".to_string()),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordStart {
            name: "demo".to_string(),
            tool_use: make_tool_use("combo_record_demo_1", "echo 2"),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordEnd {
            name: "demo".to_string(),
            tool_use_id: "combo_record_demo_1".to_string(),
            is_error: false,
            output: Final::Message("ok".to_string()),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            name: "demo".to_string(),
            starter: make_starter("demo"),
            exit_code: None,
        }));

        assert!(!combo.is_child_focused);
        assert_eq!(combo.messages.selected_idx(), None);

        combo.handle_key_event(&test_key_enter());
        assert!(combo.is_child_focused);
        assert_eq!(combo.messages.selected_idx(), Some(0));

        combo.handle_key_event(&test_key_esc());
        assert!(!combo.is_child_focused);
        assert_eq!(combo.messages.selected_idx(), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_moves_actionable_messages_with_jk() {
        let mut combo = Combo::new("demo");
        combo.handle_event(&Event::Combo(ComboEvent::Executing {
            name: "demo".to_string(),
            command_line: "demo".to_string(),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordStart {
            name: "demo".to_string(),
            tool_use: make_tool_use("combo_record_demo_0", "echo 1"),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordEnd {
            name: "demo".to_string(),
            tool_use_id: "combo_record_demo_0".to_string(),
            is_error: false,
            output: Final::Message("ok".to_string()),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordStart {
            name: "demo".to_string(),
            tool_use: make_tool_use("combo_record_demo_1", "echo 2"),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordEnd {
            name: "demo".to_string(),
            tool_use_id: "combo_record_demo_1".to_string(),
            is_error: false,
            output: Final::Message("ok".to_string()),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            name: "demo".to_string(),
            starter: make_starter("demo"),
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
        let mut combo = Combo::new("demo");
        let tool_use = make_tool_use("combo_record_demo_0", "echo 1");
        combo.handle_event(&Event::Combo(ComboEvent::Executing {
            name: "demo".to_string(),
            command_line: "demo".to_string(),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordStart {
            name: "demo".to_string(),
            tool_use,
        }));

        combo.handle_event(&Event::Combo(ComboEvent::RecordOutput {
            name: "demo".to_string(),
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
        let mut combo = Combo::new("demo");
        let tool_use = make_tool_use("combo_record_demo_0", "echo 1");
        combo.handle_event(&Event::Combo(ComboEvent::Executing {
            name: "demo".to_string(),
            command_line: "demo".to_string(),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::RecordStart {
            name: "demo".to_string(),
            tool_use,
        }));

        combo.handle_event(&Event::Combo(ComboEvent::Output {
            name: "demo".to_string(),
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
            name: "demo".to_string(),
            tool_use_id: "combo_record_demo_0".to_string(),
            is_error: false,
            output: Final::Message("ok".to_string()),
        }));

        assert!(combo.combo_stream_suppressed);
        assert_eq!(combo.state.output_chunks.len(), 1);
        assert!(combo.preview_lines.is_empty());

        combo.handle_event(&Event::Combo(ComboEvent::Output {
            name: "demo".to_string(),
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
