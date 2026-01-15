//! UI component for the run_task tool.
//!
//! Displays subagent execution with streaming output.

use bon::bon;
use coco_macro::{ComponentExt, ContentComponentExt};
use code_combo::{
    OutputChunk, ToolUse,
    tools::{Final, RunTaskInput, RunTaskOutput},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Constraint,
    prelude::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Wrap,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::prelude::*;
use tracing::warn;

use super::super::fold::FoldState;
use super::super::streaming::StreamedLines;
use super::{Component, Content, ContentComponent};
use crate::components::CacheInvalidation;
use crate::{
    actions::Action,
    components::{Persistable, ShortcutHints},
    error::*,
    events::{AnswerEvent, Event},
    global::{self, State},
    session::{self, Session},
    widgets::Paragraph,
};

#[derive(Serialize, Deserialize)]
struct Inner {
    tool_use: ToolUse,
    input: RunTaskInput,
    exec_state: ExecState,
    #[serde(default)]
    display_state: FoldState,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExecState {
    #[default]
    Initial,
    Executing {
        chunks: Vec<OutputChunk>,
    },
    Finished {
        output: RunTaskOutput,
        chunks: Vec<OutputChunk>,
    },
}

const OUTPUT_PREVIEW_LINES: usize = 8;

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "run_task")]
pub struct RunTask<'a> {
    state: State<Inner>,
    header: Paragraph<'a>,
    preview_lines: StreamedLines,
    output_text: Paragraph<'a>,
    theme_dirty: bool,
    is_focused: bool,
}

fn build_header(input: &RunTaskInput) -> Paragraph<'static> {
    let theme = global::theme();
    let lines = vec![
        Line::from(vec![
            Span::styled("Subagent: ", theme.ui.tool_title_name),
            Span::raw(input.subagent_name.clone()),
        ]),
        Line::from(vec![
            Span::styled("Task: ", theme.ui.tool_title_name),
            Span::raw(input.description.clone()),
        ]),
    ];
    Paragraph::new(lines)
}

#[bon]
impl<'a> RunTask<'a> {
    #[builder]
    pub fn try_new(tool_use: &ToolUse, output: Option<Value>) -> Result<Self> {
        let input: RunTaskInput = serde_json::from_value(tool_use.input.clone())
            .whatever_context("failed to parse RunTaskInput")?;

        let output: Option<RunTaskOutput> = output
            .map(serde_json::from_value)
            .transpose()
            .whatever_context("failed to parse RunTaskOutput")?;

        let display_state = if output.is_some() {
            FoldState::Preview
        } else {
            FoldState::Expanded
        };

        let exec_state = match output {
            Some(output) => ExecState::Finished {
                output,
                chunks: Vec::new(),
            },
            None => ExecState::Initial,
        };

        let header = build_header(&input);
        let preview_lines = StreamedLines::new(Some(OUTPUT_PREVIEW_LINES));

        let mut component = Self {
            state: State::new(Inner {
                tool_use: tool_use.to_owned(),
                input,
                exec_state,
                display_state,
            }),
            header,
            preview_lines,
            output_text: Paragraph::new(Vec::new()),
            theme_dirty: false,
            is_focused: false,
        };
        component.rebuild_output();
        Ok(component)
    }

    fn render_output(&self) -> Paragraph<'a> {
        let theme = global::theme();

        match &self.state.exec_state {
            ExecState::Initial => Paragraph::new(Vec::<Line>::new()),
            ExecState::Executing { .. } => {
                if self.preview_lines.is_empty() {
                    return Paragraph::new(vec![Line::from(Span::styled(
                        "Waiting for subagent...",
                        theme.ui.folded_hint,
                    ))]);
                }
                let lines: Vec<Line<'a>> = self
                    .preview_lines
                    .iter()
                    .map(|line| {
                        let style = match line.stream {
                            code_combo::StreamKind::Stdout => Style::default(),
                            code_combo::StreamKind::Stderr => theme.ui.bash_stderr_marker,
                        };
                        Line::from(Span::styled(line.text.clone(), style))
                    })
                    .collect();
                Paragraph::new(lines)
            }
            ExecState::Finished { output, chunks } => {
                let mut lines: Vec<Line<'a>> = Vec::new();

                // Show status
                let status_style = if output.success {
                    theme.ui.tool_title_state_completed
                } else {
                    theme.ui.tool_title_state_failed
                };
                lines.push(Line::from(vec![
                    Span::styled("Status: ", theme.ui.tool_title_name),
                    Span::styled(
                        if output.success { "Success" } else { "Failed" },
                        status_style,
                    ),
                    Span::raw(format!(" ({} turns)", output.turns)),
                ]));

                if let Some(ref error) = output.error {
                    lines.push(Line::from(vec![
                        Span::styled("Error: ", theme.ui.tool_title_state_failed),
                        Span::raw(error.clone()),
                    ]));
                }

                // Show response or streamed output
                if !output.response.is_empty() {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "Response:",
                        theme.ui.tool_title_name,
                    )));
                    for line in output.response.lines() {
                        lines.push(Line::from(line.to_string()));
                    }
                } else if !chunks.is_empty() {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "Output:",
                        theme.ui.tool_title_name,
                    )));
                    for chunk in chunks {
                        for line in &chunk.lines {
                            lines.push(Line::from(line.clone()));
                        }
                    }
                }

                Paragraph::new_wrap(lines, Wrap { trim: false })
            }
        }
    }

    fn rebuild_output(&mut self) {
        self.output_text = self.render_output();
        self.theme_dirty = false;
    }

    fn exec_output(&self) -> Option<&RunTaskOutput> {
        match &self.state.exec_state {
            ExecState::Finished { output, .. } => Some(output),
            _ => None,
        }
    }

    fn push_chunk(&mut self, chunk: OutputChunk) {
        let chunk_for_state = chunk.clone();
        let mut state = self.state.write();
        match &mut state.exec_state {
            ExecState::Executing { chunks } => chunks.push(chunk_for_state),
            ExecState::Finished { chunks, .. } => chunks.push(chunk_for_state),
            ExecState::Initial => {
                state.exec_state = ExecState::Executing {
                    chunks: vec![chunk_for_state],
                };
            }
        }
        self.preview_lines.push_chunk(&chunk);
    }

    pub fn update_output(&mut self, output: Option<Final>) -> Result<()> {
        if let Some(Final::Json(value)) = output {
            let output = serde_json::from_value::<RunTaskOutput>(value)
                .whatever_context("failed to parse RunTaskOutput")?;
            {
                let mut state = self.state.write();
                let chunks = match &mut state.exec_state {
                    ExecState::Executing { chunks } => std::mem::take(chunks),
                    ExecState::Finished { chunks, .. } => std::mem::take(chunks),
                    ExecState::Initial => Vec::new(),
                };
                state.exec_state = ExecState::Finished { output, chunks };
                state.display_state = FoldState::Preview;
            }
            self.rebuild_output();
        }
        Ok(())
    }

    fn has_output_content(&self) -> bool {
        match &self.state.exec_state {
            ExecState::Finished { output, chunks } => {
                !output.response.is_empty() || !chunks.is_empty()
            }
            ExecState::Executing { chunks } => !chunks.is_empty(),
            ExecState::Initial => false,
        }
    }

    pub fn empty_output_summary(&self) -> Option<String> {
        let output = self.exec_output()?;
        if output.response.is_empty() {
            Some(format!("{} turns, no response", output.turns))
        } else {
            None
        }
    }
}

impl<'a> Content for RunTask<'a> {
    fn height(&self, width: u16) -> usize {
        let header_height = self.header.line_count(width);

        if self.state.display_state == FoldState::Collapsed {
            return header_height;
        }

        let output_height = match self.state.display_state {
            FoldState::Preview => OUTPUT_PREVIEW_LINES.min(self.output_text.line_count(width)),
            FoldState::Expanded => self.output_text.line_count(width),
            FoldState::Collapsed => 0,
        };

        header_height + output_height
    }

    fn is_actionable(&self) -> bool {
        true
    }

    fn shortcut_hints(&self) -> ShortcutHints {
        if !self.has_output_content() {
            return ShortcutHints::default();
        }

        let toggle_text = match self.state.display_state {
            FoldState::Expanded => ("Fold", "z"),
            FoldState::Preview | FoldState::Collapsed => ("Expand", "z"),
        };

        ShortcutHints::from_visible(&[toggle_text])
    }

    fn reminder_line(&self) -> Option<Line<'static>> {
        let theme = global::theme();
        let mut spans = Vec::new();

        if let Some(summary) = self.empty_output_summary() {
            spans.push(Span::styled(format!(" - {summary}"), theme.ui.folded_hint));
        }

        if self.has_output_content() {
            match self.state.display_state {
                FoldState::Collapsed => {
                    spans.push(Span::styled(" (folded)", theme.ui.folded_hint));
                }
                FoldState::Preview => {
                    spans.push(Span::styled(" (preview)", theme.ui.folded_hint));
                }
                FoldState::Expanded => {}
            }
        }

        if spans.is_empty() {
            None
        } else {
            Some(Line::from(spans))
        }
    }
}

impl Persistable for RunTask<'static> {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: Inner = session::load(session)?;
        let preview_lines = match &state.exec_state {
            ExecState::Executing { chunks } | ExecState::Finished { chunks, .. } => {
                StreamedLines::from_chunks(chunks, Some(OUTPUT_PREVIEW_LINES))
            }
            ExecState::Initial => StreamedLines::new(Some(OUTPUT_PREVIEW_LINES)),
        };
        let header = build_header(&state.input);
        let mut component = Self {
            header,
            preview_lines,
            output_text: Paragraph::new(Vec::new()),
            state: global::State::new(state),
            theme_dirty: false,
            is_focused: false,
        };
        component.rebuild_output();
        Ok(component)
    }
}

impl Component for RunTask<'static> {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(std::iter::empty())
    }

    fn on_cache_invalidation(&mut self, reason: CacheInvalidation) {
        if matches!(reason, CacheInvalidation::Theme) {
            self.theme_dirty = true;
        }
    }

    fn handle_event(&mut self, event: &Event) {
        match event {
            Event::Answer(AnswerEvent::ToolOutput { id, chunk }) => {
                if id != &self.state.tool_use.id {
                    return;
                }
                self.push_chunk(chunk.clone());
                self.rebuild_output();
            }
            Event::Answer(AnswerEvent::ToolResult { output, .. }) => {
                if let Err(err) = self.update_output(Some(output.to_owned())) {
                    warn!(?err, "failed to update run_task output");
                };
            }
            _ => {
                handle_component_event!(self, event);
            }
        }
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        if let (KeyModifiers::NONE, KeyCode::Char('z')) = (key.modifiers, key.code) {
            if !self.has_output_content() {
                return;
            }
            let mut state = self.state.write();
            state.display_state = match state.display_state {
                FoldState::Expanded => FoldState::Collapsed,
                FoldState::Collapsed | FoldState::Preview => FoldState::Expanded,
            };
        }
    }

    fn update(&mut self, action: &Action) {
        match action {
            Action::Focus => {
                self.is_focused = true;
            }
            Action::Blur => {
                self.is_focused = false;
            }
            _ => (),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        if area.height == 0 {
            return Ok(());
        }
        if self.theme_dirty {
            self.rebuild_output();
        }

        use Constraint::Length;
        use ratatui::layout::Layout;

        let width = area.width.max(1);
        let header_height = self.header.line_count(width) as u16;

        if self.state.display_state == FoldState::Collapsed {
            let [area_header] = Layout::vertical([Length(header_height)]).areas(area);
            frame.render_widget(&self.header, area_header);
            return Ok(());
        }

        let output_height = match self.state.display_state {
            FoldState::Preview => {
                OUTPUT_PREVIEW_LINES.min(self.output_text.line_count(width)) as u16
            }
            FoldState::Expanded => self.output_text.line_count(width) as u16,
            FoldState::Collapsed => 0,
        };

        let [area_header, area_output] =
            Layout::vertical([Length(header_height), Length(output_height)]).areas(area);

        frame.render_widget(&self.header, area_header);
        frame.render_widget(&self.output_text, area_output);

        Ok(())
    }
}

impl ContentComponent for RunTask<'static> {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool_use() -> ToolUse {
        ToolUse {
            id: "tool_1".to_string(),
            name: "run_task".to_string(),
            input: json!({
                "subagent_name": "search_mcp_cli",
                "description": "Find MCP tool",
                "prompt": "Help me find a tool"
            }),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_task_parses_input() {
        let tool_use = tool_use();
        let run_task = RunTask::try_new().tool_use(&tool_use).call().unwrap();
        assert_eq!(run_task.state.input.subagent_name, "search_mcp_cli");
        assert_eq!(run_task.state.input.description, "Find MCP tool");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_task_handles_output() {
        let tool_use = tool_use();
        let output = json!({
            "success": true,
            "response": "Found the tool",
            "turns": 3
        });
        let run_task = RunTask::try_new()
            .tool_use(&tool_use)
            .output(output)
            .call()
            .unwrap();

        assert!(matches!(
            run_task.state.exec_state,
            ExecState::Finished { .. }
        ));
        if let ExecState::Finished { output, .. } = &run_task.state.exec_state {
            assert!(output.success);
            assert_eq!(output.turns, 3);
        }
    }
}
