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
use serde_json::json;

use code_combo::{
    Instruction, OutputChunk, StreamKind, ToolUse,
    tools::{BASH_TOOL_NAME, BashInput, BashOutput, Final},
};

use super::fold::FoldState;
use super::streaming::StreamedLines;
use crate::{
    actions::Action,
    components::{
        Component, Content, ContentComponent, Message, Messages, NavigationKey, NavigationResult,
        Persistable, Plain, ShortcutHints, Tool,
    },
    error::*,
    events::{AnswerEvent, ComboEvent, Event},
    global::{self, State},
    session::{self, Session},
    theme::FinalizedTheme,
    widgets::Paragraph,
};

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
    #[default]
    Messages,
    Stream,
}

#[derive(Serialize, Deserialize)]
struct Inner {
    name: String,
    is_error: bool,
    starter_state: StarterState,
    #[serde(default = "default_display_state")]
    display_state: FoldState,
    #[serde(default)]
    view: ComboView,
    #[serde(default)]
    instructions: Vec<Instruction>,
    #[serde(default)]
    output_chunks: Vec<OutputChunk>,
    #[serde(default = "default_preview_lines")]
    preview_lines: StreamedLines,
    #[serde(default)]
    error_message: Option<String>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            name: String::new(),
            is_error: false,
            starter_state: StarterState::default(),
            display_state: default_display_state(),
            view: ComboView::default(),
            instructions: Vec::new(),
            output_chunks: Vec::new(),
            preview_lines: default_preview_lines(),
            error_message: None,
        }
    }
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "combo")]
pub struct Combo {
    state: State<Inner>,
    messages: Messages,
    is_focused: bool,
    is_child_focused: bool,
}

const LIMIT: usize = 10;
const STREAM_MARKER: &str = "|";

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
            messages: Messages::default(),
            is_focused: false,
            is_child_focused: false,
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

    fn handle_tab_key(&mut self) {
        if !self.can_focus_messages() {
            return;
        }
        if !self.is_child_focused {
            if self.messages.select_first_actionable() {
                self.is_child_focused = true;
            }
            return;
        }
        if self.messages.select_next_actionable() {
            return;
        }
        self.clear_child_focus();
    }

    fn on_combo_event(&mut self, event: &ComboEvent) {
        match event {
            ComboEvent::NotFound { name } => {
                if &self.state.name == name {
                    self.state.write().starter_state = StarterState::NotFound
                }
            }
            ComboEvent::Output { name, chunk } => self.on_ouput_event(name, chunk),
            ComboEvent::Preview { name, instructions } => {
                if &self.state.name == name {
                    self.replace_instructions(instructions);
                }
            }
            ComboEvent::Executing { name } => {
                if &self.state.name == name {
                    self.clear_child_focus();
                    let mut state = self.state.write();
                    state.starter_state = StarterState::Executing;
                    state.is_error = false;
                    state.error_message = None;
                    state.view = ComboView::Messages;
                    state.instructions.clear();
                    state.output_chunks.clear();
                    state.preview_lines = StreamedLines::new(Some(LIMIT));
                    state.display_state.expand();
                    drop(state);
                    self.messages.clear();
                }
            }
            ComboEvent::Executed { name, starter, .. } => {
                if &self.state.name == name {
                    {
                        let mut state = self.state.write();
                        let instructions = match &starter.combo {
                            Ok(combo) => {
                                state.is_error = false;
                                state.error_message = None;
                                combo.instructions.clone()
                            }
                            Err(err) => {
                                state.is_error = true;
                                state.error_message =
                                    Some(format!("Failed to execute starter: {err}"));
                                Vec::new()
                            }
                        };
                        state.instructions = instructions;
                        state.starter_state = StarterState::Finalized;
                        state.display_state.expand();
                    }
                    self.rebuild_messages();
                }
            }
            ComboEvent::Cancelled { name } => {
                if name
                    .as_ref()
                    .map(|name| name == &self.state.name)
                    .unwrap_or(true)
                {
                    self.clear_child_focus();
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

    fn on_ouput_event(&mut self, name: &str, chunk: &OutputChunk) {
        if self.state.name != name {
            return;
        }
        let mut state = self.state.write();
        state.output_chunks.push(chunk.clone());
        state.preview_lines.push_chunk(chunk);
    }

    fn has_message_content(&self) -> bool {
        !self.state.instructions.is_empty() || self.state.error_message.is_some()
    }

    fn has_body_content(&self) -> bool {
        self.has_message_content() || self.has_stream_content()
    }

    fn has_stream_content(&self) -> bool {
        !self.state.output_chunks.is_empty() || !self.state.preview_lines.is_empty()
    }

    fn can_toggle_view(&self) -> bool {
        self.has_stream_content()
    }

    fn toggle_view(&mut self) {
        self.clear_child_focus();
        let mut state = self.state.write();
        state.view = match state.view {
            ComboView::Messages => ComboView::Stream,
            ComboView::Stream => ComboView::Messages,
        };
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

    fn replace_instructions(&mut self, instructions: &[Instruction]) {
        {
            let mut state = self.state.write();
            state.instructions = instructions.to_vec();
        }
        self.rebuild_messages();
    }

    fn rebuild_messages(&mut self) {
        self.clear_child_focus();
        let (name, instructions, error_message) = {
            let state = self.state.read();
            (
                state.name.clone(),
                state.instructions.clone(),
                state.error_message.clone(),
            )
        };
        let mut messages = build_instruction_messages(&name, &instructions);
        if let Some(message) = error_message {
            messages.push(Message::system(Plain::new(message).into()));
        }
        self.messages.clear();
        self.messages.extend(messages.into_iter());
    }

    fn messages_body_height(&self, width: u16) -> u16 {
        let messages_height =
            u16::try_from(self.messages.height_for_width(width)).unwrap_or(u16::MAX);
        let preview_height = if matches!(self.state.starter_state, StarterState::Executing) {
            self.state.preview_lines.len() as u16
        } else {
            0
        };
        let spacing = if messages_height > 0 && preview_height > 0 {
            1
        } else {
            0
        };
        messages_height
            .saturating_add(spacing)
            .saturating_add(preview_height)
    }

    fn stream_line_count(&self) -> u16 {
        let total = self
            .state
            .output_chunks
            .iter()
            .map(|chunk| chunk.lines.len())
            .sum::<usize>();
        u16::try_from(total).unwrap_or(u16::MAX)
    }

    fn draw_messages_view(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        if area.height == 0 {
            return Ok(());
        }
        let messages_height =
            u16::try_from(self.messages.height_for_width(area.width)).unwrap_or(u16::MAX);
        let preview_height = if matches!(self.state.starter_state, StarterState::Executing) {
            self.state.preview_lines.len() as u16
        } else {
            0
        };
        let spacing = if messages_height > 0 && preview_height > 0 {
            1
        } else {
            0
        };

        let mut rows = Vec::new();
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

    fn draw_stream_view(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        if area.height == 0 {
            return Ok(());
        }
        let (output, markers) = self.render_stream_lines();
        self.draw_stream_section(frame, area, &output, markers.as_ref());
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
        for line in self.state.preview_lines.iter() {
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

    fn render_stream_lines(&self) -> (Paragraph<'static>, Option<Paragraph<'static>>) {
        let theme = global::theme();
        let mut lines = Vec::new();
        let mut markers = Vec::new();
        for chunk in &self.state.output_chunks {
            let marker_style = match chunk.stream {
                StreamKind::Stdout => theme.ui.bash_stdout_marker,
                StreamKind::Stderr => theme.ui.bash_stderr_marker,
            };
            for line in &chunk.lines {
                lines.push(Line::from(line.clone()));
                markers.push(Line::from(Span::styled(STREAM_MARKER, marker_style)));
            }
        }
        let output = Paragraph::new(lines);
        let markers = if markers.is_empty() {
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
        let border_height = 1;
        if self.has_collapsible_body() && self.state.display_state.is_collapsed() {
            return border_height;
        }
        let body_height = match self.state.view {
            ComboView::Messages => self.messages_body_height(width),
            ComboView::Stream => self.stream_line_count(),
        };
        body_height as usize + border_height
    }

    fn is_actionable(&self) -> bool {
        self.has_collapsible_body() || self.can_toggle_view() || self.can_focus_messages()
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
            let tab_text = if self.is_child_focused {
                ("Next", "Tab")
            } else {
                ("Select", "Tab")
            };
            hints.push_visible(&[tab_text]);
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
            hints.push_visible(&[("View", "v")]);
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
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: Inner = session::load(session)?;
        let mut combo = Self {
            state: State::new(state),
            messages: Messages::default(),
            is_focused: false,
            is_child_focused: false,
        };
        combo.rebuild_messages();
        Ok(combo)
    }
}

impl Component for Combo {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(vec![&mut self.messages as &mut dyn Component].into_iter())
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        if let (KeyModifiers::NONE, KeyCode::Tab) = (key.modifiers, key.code) {
            self.handle_tab_key();
            return;
        }

        if self.is_child_focused {
            self.messages.handle_key_event(key);
            return;
        }

        if !self.has_collapsible_body() && !self.can_toggle_view() && !self.can_focus_messages() {
            return;
        }

        if let (KeyModifiers::NONE, KeyCode::Char('v')) = (key.modifiers, key.code) {
            if self.can_toggle_view() {
                self.toggle_view();
            }
            return;
        }

        if let (KeyModifiers::NONE, KeyCode::Char('z')) = (key.modifiers, key.code)
            && self.has_collapsible_body()
        {
            self.toggle_display_state();
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
            return Ok(());
        }

        let output_area = block.inner(area);

        match self.state.view {
            ComboView::Messages => self.draw_messages_view(frame, output_area)?,
            ComboView::Stream => self.draw_stream_view(frame, output_area)?,
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

fn build_instruction_messages(name: &str, instructions: &[Instruction]) -> Vec<Message> {
    instructions
        .iter()
        .enumerate()
        .map(|(idx, instruction)| match instruction {
            Instruction::Text(text) => Message::user(Plain::new(text.clone()).into()),
            Instruction::Command { input, output } => {
                build_command_message(name, idx, input, output)
            }
        })
        .collect()
}

fn build_command_message(
    name: &str,
    idx: usize,
    input: &BashInput,
    output: &BashOutput,
) -> Message {
    let tool_use = ToolUse {
        id: format!("combo_preview_{name}_{idx}"),
        name: BASH_TOOL_NAME.to_string(),
        input: serde_json::to_value(input).unwrap_or_else(|_| {
            json!({
                "command": input.command,
                "timeout": input.timeout,
            })
        }),
    };
    let is_error = output.exit_code != 0;
    let output_value = serde_json::to_value(output).unwrap_or_else(|_| {
        json!({
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "timed_out": output.timed_out,
        })
    });
    let mut tool = Tool::new(tool_use.clone());
    tool.handle_event(&Event::Answer(AnswerEvent::ToolResult {
        id: tool_use.id.clone(),
        is_error,
        is_user_cancelled: false,
        output: Final::Json(output_value),
    }));
    Message::bot(tool.into())
}

#[cfg(test)]
mod tests {
    use crate::actions::Action;
    use crate::events::{ComboEvent, Event};

    use super::*;
    use code_combo::tools::{BashInput, BashOutput};

    fn test_key_z() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE)
    }

    fn test_key_tab() -> KeyEvent {
        KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)
    }

    fn make_command_instruction(command: &str) -> code_combo::Instruction {
        code_combo::Instruction::Command {
            input: BashInput::new(command.to_string()),
            output: BashOutput {
                exit_code: 0,
                stdout: "ok".to_string(),
                stderr: String::new(),
                timed_out: false,
            },
        }
    }

    fn make_starter(name: &str) -> code_combo::Starter {
        code_combo::Starter {
            path: "/tmp/demo".to_string(),
            combo: Ok(code_combo::Combo {
                metadata: code_combo::ComboMetadata {
                    name: name.to_string(),
                    description: String::new(),
                    mode: code_combo::ComboMode::Unknown,
                },
                instructions: vec![code_combo::Instruction::Text(
                    "line1\nline2\nline3".to_string(),
                )],
            }),
        }
    }

    fn make_starter_with_commands(name: &str, commands: &[&str]) -> code_combo::Starter {
        let instructions = commands
            .iter()
            .map(|command| make_command_instruction(command))
            .collect::<Vec<_>>();
        code_combo::Starter {
            path: "/tmp/demo".to_string(),
            combo: Ok(code_combo::Combo {
                metadata: code_combo::ComboMetadata {
                    name: name.to_string(),
                    description: String::new(),
                    mode: code_combo::ComboMode::Unknown,
                },
                instructions,
            }),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_is_collapsed_by_default_and_toggles_with_z() {
        let mut combo = Combo::new("demo");
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            name: "demo".to_string(),
            starter: make_starter("demo"),
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
        }));
        combo.handle_event(&Event::Combo(ComboEvent::Output {
            name: "demo".to_string(),
            chunk: code_combo::OutputChunk {
                timestamp: 0,
                stream: code_combo::StreamKind::Stdout,
                lines: vec!["line1".to_string(), "line2".to_string()],
            },
        }));

        assert_eq!(combo.height(80), 3);
        combo.handle_key_event(&test_key_z());
        assert_eq!(combo.height(80), 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_persists_collapsed_state() {
        let mut combo = Combo::new("demo");
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            name: "demo".to_string(),
            starter: make_starter("demo"),
        }));
        combo.handle_action(&Action::Blur);
        combo.handle_key_event(&test_key_z());
        assert!(combo.height(80) > 1);

        let session = combo.save();
        let loaded = Combo::load(session).unwrap();
        assert!(loaded.height(80) > 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_cycles_actionable_messages_with_tab() {
        let mut combo = Combo::new("demo");
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            name: "demo".to_string(),
            starter: make_starter_with_commands("demo", &["echo 1", "echo 2"]),
        }));

        assert!(!combo.is_child_focused);
        assert_eq!(combo.messages.selected_idx(), None);

        combo.handle_key_event(&test_key_tab());
        assert!(combo.is_child_focused);
        assert_eq!(combo.messages.selected_idx(), Some(0));

        combo.handle_key_event(&test_key_tab());
        assert!(combo.is_child_focused);
        assert_eq!(combo.messages.selected_idx(), Some(1));

        combo.handle_key_event(&test_key_tab());
        assert!(!combo.is_child_focused);
        assert_eq!(combo.messages.selected_idx(), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_moves_actionable_messages_with_jk() {
        let mut combo = Combo::new("demo");
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            name: "demo".to_string(),
            starter: make_starter_with_commands("demo", &["echo 1", "echo 2"]),
        }));

        combo.handle_key_event(&test_key_tab());
        assert_eq!(combo.messages.selected_idx(), Some(0));

        combo.handle_navigation(NavigationKey::Down);
        assert_eq!(combo.messages.selected_idx(), Some(1));

        combo.handle_navigation(NavigationKey::Up);
        assert_eq!(combo.messages.selected_idx(), Some(0));
    }
}
