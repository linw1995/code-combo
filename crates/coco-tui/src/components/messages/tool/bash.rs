use bon::bon;
use coco_macro::{ComponentExt, ContentComponentExt};
use code_combo::{
    OutputChunk, ToolUse,
    tools::{BashInput, BashOutput, Final},
};
use code_highlight::Lang;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    prelude::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Wrap},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::prelude::*;
use tracing::warn;

use super::super::fold::FoldState;
use super::super::streaming::StreamedLines;
use super::{Component, Content, ContentComponent};
use crate::{
    actions::{Action, ToolAction},
    components::{CodeHighlight, Persistable, shortcuts_desc},
    error::*,
    events::{AnswerEvent, AskEvent, Event},
    global::{self, State},
    session::{self, Session},
    widgets::Paragraph,
};

#[derive(Serialize, Deserialize)]
struct Inner {
    tool_use: ToolUse,
    requiring_confirmation: bool,
    output: Option<BashOutput>,
    chunks: Vec<OutputChunk>,
    #[serde(default)]
    view: BashOutputView,
    #[serde(default)]
    display_state: FoldState,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "bash")]
pub struct Bash<'a> {
    state: State<Inner>,

    input: CodeHighlight<'a>,
    streamed_lines: StreamedLines,
    output_text: Paragraph<'a>,
    output_markers: Option<Paragraph<'a>>,
    output_preview_text: Paragraph<'a>,
    output_preview_markers: Option<Paragraph<'a>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BashOutputView {
    Stdout,
    Stderr,
    #[default]
    Mixed,
}

impl BashOutputView {
    fn index(self) -> usize {
        match self {
            Self::Stdout => 0,
            Self::Stderr => 1,
            Self::Mixed => 2,
        }
    }
}

const OUTPUT_PREVIEW_LINES: usize = 6;
const TAB_PANEL_HEIGHT: usize = 3;

fn tab_panel_width(total_width: u16) -> u16 {
    let min_total_width = 24u16;
    if total_width < min_total_width {
        return 0;
    }
    3
}

fn output_marker_width(view: BashOutputView) -> u16 {
    match view {
        BashOutputView::Mixed => 1,
        _ => 0,
    }
}

fn render_tabs_panel(view: BashOutputView) -> Paragraph<'static> {
    let theme = global::theme();
    let highlight = theme.ui.bash_tab_active;
    let items = [
        (BashOutputView::Stdout, " 1 ", theme.ui.bash_tab_stdout),
        (BashOutputView::Stderr, " 2 ", theme.ui.bash_tab_stderr),
        (BashOutputView::Mixed, " 3 ", theme.ui.bash_tab_mixed),
    ];
    let lines = items
        .into_iter()
        .map(|(v, digit, base_style)| {
            let style = if v.index() == view.index() {
                highlight
            } else {
                Style::default()
            };
            Line::from(Span::styled(digit, base_style.patch(style)))
        })
        .collect::<Vec<_>>();
    Paragraph::new(lines)
}

fn limit_tail_lines<T>(mut lines: Vec<T>, max_lines: Option<usize>) -> Vec<T> {
    let Some(max_lines) = max_lines else {
        return lines;
    };
    if lines.len() > max_lines {
        let start = lines.len() - max_lines;
        lines = lines.split_off(start);
    }
    lines
}

const OUTPUT_MARKER: &str = "▐";

fn build_output_view<'a>(
    output: Option<&BashOutput>,
    chunks: &[OutputChunk],
    streamed_lines: &StreamedLines,
    view: BashOutputView,
    max_lines: Option<usize>,
) -> (Paragraph<'a>, Option<Paragraph<'a>>) {
    let theme = global::theme();
    let mut lines: Vec<Line<'a>> = Vec::new();
    let Some(output) = output else {
        return (
            Paragraph::new_wrap(lines, Wrap { trim: false }),
            if view == BashOutputView::Mixed {
                Some(Paragraph::new(Vec::<Line>::new()))
            } else {
                None
            },
        );
    };

    match view {
        BashOutputView::Stdout => {
            for line in output.stdout.lines() {
                lines.push(Line::from(line.to_string()));
            }
            let lines = limit_tail_lines(lines, max_lines);
            (Paragraph::new_wrap(lines, Wrap { trim: false }), None)
        }
        BashOutputView::Stderr => {
            for line in output.stderr.lines() {
                lines.push(Line::from(line.to_string()));
            }
            let lines = limit_tail_lines(lines, max_lines);
            (Paragraph::new_wrap(lines, Wrap { trim: false }), None)
        }
        BashOutputView::Mixed => {
            let mut markers: Vec<Line<'a>> = Vec::new();
            if !streamed_lines.is_empty() {
                for line in streamed_lines.iter() {
                    let marker_style = match line.stream {
                        code_combo::StreamKind::Stdout => theme.ui.bash_stdout_marker,
                        code_combo::StreamKind::Stderr => theme.ui.bash_stderr_marker,
                    };
                    lines.push(Line::from(line.text.clone()));
                    markers.push(Line::from(Span::styled(OUTPUT_MARKER, marker_style)));
                }
            } else if chunks.is_empty() {
                for (marker_style, text) in [
                    (theme.ui.bash_stderr_marker, &output.stderr),
                    (theme.ui.bash_stdout_marker, &output.stdout),
                ] {
                    if text.is_empty() {
                        continue;
                    }
                    for line in text.lines() {
                        lines.push(Line::from(line.to_string()));
                        markers.push(Line::from(Span::styled(OUTPUT_MARKER, marker_style)));
                    }
                }
            } else {
                for chunk in chunks {
                    let marker_style = match chunk.stream {
                        code_combo::StreamKind::Stdout => theme.ui.bash_stdout_marker,
                        code_combo::StreamKind::Stderr => theme.ui.bash_stderr_marker,
                    };
                    for line in &chunk.lines {
                        lines.push(Line::from(line.clone()));
                        markers.push(Line::from(Span::styled(OUTPUT_MARKER, marker_style)));
                    }
                }
            }
            let lines = limit_tail_lines(lines, max_lines);
            let markers = limit_tail_lines(markers, max_lines);
            (Paragraph::new(lines), Some(Paragraph::new(markers)))
        }
    }
}

fn generate_input<'b>(tool_use: &ToolUse) -> CodeHighlight<'b> {
    let input: BashInput =
        serde_json::from_value(tool_use.input.clone()).expect("failed to parse BashInput");
    CodeHighlight::try_new(&input.command, Lang::Bash).expect("failed to new CodeHighlight")
}

#[bon]
impl<'a> Bash<'a> {
    #[builder]
    pub fn try_new(tool_use: &ToolUse, output: Option<Value>) -> Result<Self> {
        let input = generate_input(tool_use);
        let output: Option<BashOutput> = output
            .map(serde_json::from_value)
            .transpose()
            .whatever_context("failed to parse BashOutput")?;
        let chunks = Vec::new();
        let streamed_lines = StreamedLines::new(None);
        let (output_text, output_markers) = build_output_view(
            output.as_ref(),
            &chunks,
            &streamed_lines,
            BashOutputView::default(),
            None,
        );
        let (output_preview_text, output_preview_markers) = build_output_view(
            output.as_ref(),
            &chunks,
            &streamed_lines,
            BashOutputView::default(),
            Some(OUTPUT_PREVIEW_LINES),
        );
        let display_state = display_state_for_output(output.as_ref(), FoldState::Preview);

        Ok(Self {
            state: State::new(Inner {
                tool_use: tool_use.to_owned(),
                requiring_confirmation: false,
                output,
                chunks,
                view: BashOutputView::default(),
                display_state,
            }),
            input,
            streamed_lines,
            output_text,
            output_markers,
            output_preview_text,
            output_preview_markers,
        })
    }

    fn rebuild_output(&mut self) {
        let (output_text, output_markers) = build_output_view(
            self.state.output.as_ref(),
            &self.state.chunks,
            &self.streamed_lines,
            self.state.view,
            None,
        );
        let (output_preview_text, output_preview_markers) = build_output_view(
            self.state.output.as_ref(),
            &self.state.chunks,
            &self.streamed_lines,
            self.state.view,
            Some(OUTPUT_PREVIEW_LINES),
        );
        self.output_text = output_text;
        self.output_markers = output_markers;
        self.output_preview_text = output_preview_text;
        self.output_preview_markers = output_preview_markers;
    }

    pub fn update_output(&mut self, output: Option<Final>) -> Result<()> {
        if let Some(Final::Json(value)) = output {
            let output = serde_json::from_value::<BashOutput>(value)
                .whatever_context("failed to parse BashOutput")?;
            {
                let mut state = self.state.write();
                state.output = Some(output);
            }
            self.streamed_lines = StreamedLines::from_chunks(&self.state.chunks, None);
            self.rebuild_output();
        }
        Ok(())
    }

    fn has_output_content(&self) -> bool {
        match self.state.output.as_ref() {
            Some(output) => {
                !(output.stdout.is_empty()
                    && output.stderr.is_empty()
                    && self.state.chunks.is_empty())
            }
            None => !self.state.chunks.is_empty(),
        }
    }

    pub fn empty_output_summary(&self) -> Option<String> {
        if self.has_output_content() {
            return None;
        }
        let output = self.state.output.as_ref()?;
        if output.timed_out {
            return Some(format!("Timed out (exit {}, no output)", output.exit_code));
        }
        Some(format!("Exit {} (no output)", output.exit_code))
    }
}

impl<'a> Content for Bash<'a> {
    fn height(&self, width: u16) -> usize {
        if self.state.requiring_confirmation
            || self.state.display_state == FoldState::Collapsed
            || !self.has_output_content()
        {
            return self.input.height(width);
        }
        let height_input = self.input.height(width);
        let min_height = TAB_PANEL_HEIGHT;

        let width_tab = if self.state.display_state == FoldState::Preview {
            0
        } else {
            tab_panel_width(width)
        };
        let width_body = width.saturating_sub(width_tab).max(1);
        let width_marker = output_marker_width(self.state.view);
        let width_text = width_body.saturating_sub(width_marker).max(1);
        let height_output = match self.state.display_state {
            FoldState::Preview => OUTPUT_PREVIEW_LINES.max(min_height),
            FoldState::Expanded => self.output_text.line_count(width_text).max(min_height),
            FoldState::Collapsed => 0,
        };

        height_input + height_output
    }

    fn is_actionable(&self) -> bool {
        true
    }

    fn block_with_shortcuts_desc<'b>(&self, mut block: Block<'b>) -> Block<'b> {
        if self.state.requiring_confirmation {
            return block
                .title_bottom(shortcuts_desc(&[("Run", "CR"), ("Allow in Session", "A")]))
                .title_bottom(shortcuts_desc(&[("Cancel", "Esc")]));
        }

        if !self.has_output_content() {
            return block;
        }

        let toggle_text = match self.state.display_state {
            FoldState::Expanded => ("Fold", "z"),
            FoldState::Preview | FoldState::Collapsed => ("Expand", "z"),
        };

        if matches!(self.state.display_state, FoldState::Expanded) {
            let view = match self.state.view {
                BashOutputView::Stdout => "Stdout",
                BashOutputView::Stderr => "Stderr",
                BashOutputView::Mixed => "Mixed",
            };
            block = block.title_bottom(shortcuts_desc(&[(view, "1/2/3")]));
        }

        block.title_bottom(shortcuts_desc(&[toggle_text]))
    }

    fn reminder_line(&self) -> Option<Line<'static>> {
        let mut spans = Vec::new();
        if let Some(summary) = self.empty_output_summary() {
            spans.push(Span::raw(format!(" - {summary}")).dark_gray());
        }
        if self.has_output_content() {
            match self.state.display_state {
                FoldState::Collapsed => spans.push(Span::raw(" (folded)").dark_gray()),
                FoldState::Preview => spans.push(Span::raw(" (preview)").dark_gray()),
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

impl Persistable for Bash<'static> {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: Inner = session::load(session)?;
        let streamed_lines = StreamedLines::from_chunks(&state.chunks, None);
        let (output_text, output_markers) = build_output_view(
            state.output.as_ref(),
            &state.chunks,
            &streamed_lines,
            state.view,
            None,
        );
        let (output_preview_text, output_preview_markers) = build_output_view(
            state.output.as_ref(),
            &state.chunks,
            &streamed_lines,
            state.view,
            Some(OUTPUT_PREVIEW_LINES),
        );
        Ok(Self {
            input: generate_input(&state.tool_use),
            streamed_lines,
            output_text,
            output_markers,
            output_preview_text,
            output_preview_markers,
            state: global::State::new(state),
        })
    }
}

impl Component for Bash<'static> {
    fn handle_event(&mut self, event: &Event) {
        match event {
            Event::Ask(AskEvent::ToolUsePermission(_)) => {
                let mut state = self.state.write();
                state.requiring_confirmation = true;
                state.display_state.preview();
            }
            Event::Answer(AnswerEvent::ToolOutput { id, chunk }) => {
                if id != &self.state.tool_use.id {
                    return;
                }
                let mut state = self.state.write();
                let output = state.output.get_or_insert_with(|| BashOutput {
                    exit_code: 255,
                    stdout: String::new(),
                    stderr: String::new(),
                    timed_out: false,
                });
                for line in &chunk.lines {
                    match chunk.stream {
                        code_combo::StreamKind::Stdout => {
                            output.stdout.push_str(line);
                            output.stdout.push('\n');
                        }
                        code_combo::StreamKind::Stderr => {
                            output.stderr.push_str(line);
                            output.stderr.push('\n');
                        }
                    }
                }
                state.chunks.push(chunk.clone());
                self.streamed_lines.push_chunk(chunk);
                drop(state);
                self.rebuild_output();
            }
            Event::Answer(AnswerEvent::ToolResult { output, .. }) => {
                if let Err(err) = self.update_output(Some(output.to_owned())) {
                    warn!(?err, "failed to update tool output");
                };
                let mut state = self.state.write();
                state.display_state =
                    display_state_for_output(state.output.as_ref(), state.display_state);
                state.requiring_confirmation = false;
            }
            _ => (),
        }
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        match (self.state.display_state, key.modifiers, key.code) {
            (FoldState::Expanded, KeyModifiers::NONE, KeyCode::Char('1')) => {
                if !self.has_output_content() {
                    return;
                }
                let mut state = self.state.write();
                state.view = BashOutputView::Stdout;
                drop(state);
                self.rebuild_output();
            }
            (FoldState::Expanded, KeyModifiers::NONE, KeyCode::Char('2')) => {
                if !self.has_output_content() {
                    return;
                }
                let mut state = self.state.write();
                state.view = BashOutputView::Stderr;
                state.display_state.expand();
                drop(state);
                self.rebuild_output();
            }
            (FoldState::Expanded, KeyModifiers::NONE, KeyCode::Char('3')) => {
                if !self.has_output_content() {
                    return;
                }
                let mut state = self.state.write();
                state.view = BashOutputView::Mixed;
                drop(state);
                self.rebuild_output();
            }
            (_, KeyModifiers::NONE, KeyCode::Char('z')) => {
                if !self.has_output_content() {
                    return;
                }
                let mut state = self.state.write();
                state.display_state = match state.display_state {
                    FoldState::Expanded => FoldState::Collapsed,
                    FoldState::Collapsed | FoldState::Preview => FoldState::Expanded,
                };
            }
            (_, KeyModifiers::NONE, KeyCode::Enter) => {
                if !self.state.requiring_confirmation {
                    return;
                }
                self.state.write().display_state.preview();
                global::action_tx()
                    .send(ToolAction::Grant(self.state.tool_use.to_owned()).into())
                    .unwrap();
                self.state.write().requiring_confirmation = false;
            }
            (_, KeyModifiers::NONE, KeyCode::Char('a') | KeyCode::Char('A')) => {
                if !self.state.requiring_confirmation {
                    return;
                }
                self.state.write().display_state.preview();
                global::action_tx()
                    .send(ToolAction::GrantSession(self.state.tool_use.to_owned()).into())
                    .unwrap();
                self.state.write().requiring_confirmation = false;
            }
            (_, KeyModifiers::NONE, KeyCode::Esc) => {
                if !self.state.requiring_confirmation {
                    return;
                }
                global::action_tx()
                    .send(ToolAction::Cancel(self.state.tool_use.to_owned()).into())
                    .unwrap();
                self.state.write().requiring_confirmation = false;
            }
            _ => (), // ignore
        }
    }

    fn update(&mut self, action: &Action) {
        if !matches!(action, Action::Blur) {
            return;
        }
        if self.state.requiring_confirmation {
            return;
        }
        if !self.has_output_content() {
            return;
        }
        if self.state.display_state != FoldState::Preview {
            return;
        }
        let Some(output) = self.state.output.as_ref() else {
            return;
        };
        if output.exit_code != 0 {
            return;
        }
        self.state.write().display_state.collapse();
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        if area.height == 0 {
            return Ok(());
        }

        use Constraint::Length;

        let width = area.width.max(1);
        let height_input = self.input.height(width);

        if self.state.display_state == FoldState::Collapsed
            || self.state.requiring_confirmation
            || !self.has_output_content()
        {
            let [area_input] = Layout::vertical([Length(height_input as u16)]).areas(area);
            self.input.draw(frame, area_input)?;
            return Ok(());
        }

        let width_tabs = if self.state.display_state == FoldState::Preview {
            0
        } else {
            tab_panel_width(width)
        };
        let width_marker = output_marker_width(self.state.view);
        let width_body = width.saturating_sub(width_tabs).max(1);
        let width_text = width_body.saturating_sub(width_marker).max(1);
        let min_height = TAB_PANEL_HEIGHT;
        let height_output = match self.state.display_state {
            FoldState::Preview => OUTPUT_PREVIEW_LINES.max(min_height),
            FoldState::Expanded => self.output_text.line_count(width_text).max(min_height),
            FoldState::Collapsed => 0,
        };
        let [area_input, area_output] =
            Layout::vertical([Length(height_input as u16), Length(height_output as u16)])
                .areas(area);

        self.input.draw(frame, area_input)?;

        let (area_output_view, area_output_tabs) = if width_tabs == 0 {
            (area_output, None)
        } else {
            let [view, tabs] =
                Layout::horizontal([Constraint::Min(1), Constraint::Length(width_tabs)])
                    .areas(area_output);
            (view, Some(tabs))
        };

        let (output_text, output_markers) = match self.state.display_state {
            FoldState::Preview => (&self.output_preview_text, &self.output_preview_markers),
            FoldState::Expanded => (&self.output_text, &self.output_markers),
            FoldState::Collapsed => (&self.output_text, &self.output_markers),
        };

        if width_marker == 0 {
            frame.render_widget(output_text, area_output_view);
        } else {
            let [area_text, area_markers] =
                Layout::horizontal([Constraint::Min(1), Constraint::Length(width_marker)])
                    .areas(area_output_view);
            frame.render_widget(output_text, area_text);
            if let Some(markers) = output_markers {
                frame.render_widget(markers, area_markers);
            }
        }

        if let Some(area_tabs) = area_output_tabs {
            let tabs_panel = render_tabs_panel(self.state.view);
            frame.render_widget(tabs_panel, area_tabs);
        }
        Ok(())
    }
}

impl ContentComponent for Bash<'static> {}

fn display_state_for_output(output: Option<&BashOutput>, current: FoldState) -> FoldState {
    let Some(output) = output else {
        return current;
    };
    if output.exit_code != 0 {
        return FoldState::Expanded;
    }
    if current == FoldState::Expanded {
        FoldState::Expanded
    } else {
        FoldState::Preview
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::actions::Action;
    use crate::events::Event;

    fn tool_use() -> ToolUse {
        ToolUse {
            id: "tool_1".to_string(),
            name: "bash".to_string(),
            input: json!({
                "command": "echo hi",
                "timeout": 1000,
            }),
        }
    }

    fn bash_output_with_exit(exit_code: u8) -> BashOutput {
        BashOutput {
            exit_code,
            stdout: "out\n".to_string(),
            stderr: "err\n".to_string(),
            timed_out: false,
        }
    }

    fn bash_output() -> BashOutput {
        bash_output_with_exit(0)
    }

    fn bash_output_empty() -> BashOutput {
        BashOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bash_auto_collapses_on_success_and_stays_open_on_failure() {
        let tool_use = tool_use();
        let mut bash = Bash::try_new().tool_use(&tool_use).call().unwrap();
        assert_eq!(bash.state.display_state, FoldState::Preview);

        let value = serde_json::to_value(bash_output()).unwrap();
        bash.handle_event(&Event::Answer(AnswerEvent::ToolResult {
            id: "tool_1".to_string(),
            is_error: false,
            is_user_cancelled: false,
            output: Final::Json(value),
        }));
        assert_eq!(bash.state.display_state, FoldState::Preview);
        bash.update(&Action::Blur);
        assert_eq!(bash.state.display_state, FoldState::Collapsed);

        let value = serde_json::to_value(bash_output_with_exit(1)).unwrap();
        bash.handle_event(&Event::Answer(AnswerEvent::ToolResult {
            id: "tool_1".to_string(),
            is_error: true,
            is_user_cancelled: false,
            output: Final::Json(value),
        }));
        assert_eq!(bash.state.display_state, FoldState::Expanded);
        bash.update(&Action::Blur);
        assert_eq!(bash.state.display_state, FoldState::Expanded);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bash_switch_view_unfolds() {
        let tool_use = tool_use();
        let value = serde_json::to_value(bash_output()).unwrap();
        let mut bash = Bash::try_new()
            .tool_use(&tool_use)
            .output(value)
            .call()
            .unwrap();

        bash.state.write().display_state = FoldState::Collapsed;
        bash.handle_key_event(&key(KeyCode::Char('1')));
        assert_eq!(bash.state.display_state, FoldState::Collapsed);

        bash.state.write().display_state = FoldState::Expanded;
        bash.handle_key_event(&key(KeyCode::Char('1')));
        assert_eq!(bash.state.display_state, FoldState::Expanded);
        assert_eq!(bash.state.view.index(), 0);

        bash.handle_key_event(&key(KeyCode::Char('2')));
        assert_eq!(bash.state.display_state, FoldState::Expanded);
        assert_eq!(bash.state.view.index(), 1);

        bash.handle_key_event(&key(KeyCode::Char('3')));
        assert_eq!(bash.state.display_state, FoldState::Expanded);
        assert_eq!(bash.state.view.index(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bash_preview_expands_on_toggle() {
        let tool_use = tool_use();
        let value = serde_json::to_value(bash_output()).unwrap();
        let mut bash = Bash::try_new()
            .tool_use(&tool_use)
            .output(value)
            .call()
            .unwrap();

        assert_eq!(bash.state.display_state, FoldState::Preview);
        bash.handle_key_event(&key(KeyCode::Char('z')));
        assert_eq!(bash.state.display_state, FoldState::Expanded);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bash_appends_streaming_chunks() {
        let tool_use = tool_use();
        let mut bash = Bash::try_new().tool_use(&tool_use).call().unwrap();

        bash.handle_event(&Event::Answer(AnswerEvent::ToolOutput {
            id: "tool_1".to_string(),
            chunk: code_combo::OutputChunk {
                timestamp: 0,
                stream: code_combo::StreamKind::Stdout,
                lines: vec!["out1".to_string(), "out2".to_string()],
            },
        }));

        let output = bash.state.output.clone().unwrap();
        assert_eq!(output.stdout, "out1\nout2\n");
        assert_eq!(output.stderr, "");
        assert_eq!(bash.state.chunks.len(), 1);
        assert_eq!(
            bash.state.chunks[0].lines,
            vec!["out1".to_string(), "out2".to_string()]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bash_hides_tabs_while_requiring_confirmation() {
        let tool_use = tool_use();
        let value = serde_json::to_value(bash_output()).unwrap();
        let mut bash = Bash::try_new()
            .tool_use(&tool_use)
            .output(value)
            .call()
            .unwrap();

        let height_with_output = bash.height(80);
        bash.handle_event(&Event::Ask(AskEvent::ToolUsePermission(
            "tool_1".to_string(),
        )));
        let height_confirm = bash.height(80);

        assert!(height_with_output > height_confirm);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bash_collapsed_shows_input_only() {
        let tool_use = tool_use();
        let value = serde_json::to_value(bash_output()).unwrap();
        let mut bash = Bash::try_new()
            .tool_use(&tool_use)
            .output(value)
            .call()
            .unwrap();

        bash.state.write().display_state = FoldState::Collapsed;
        let height = bash.height(80);
        let input_height = bash.input.height(80);

        assert_eq!(height, input_height);
        assert!(height > 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bash_no_output_does_not_auto_collapse() {
        let tool_use = tool_use();
        let value = serde_json::to_value(bash_output_empty()).unwrap();
        let mut bash = Bash::try_new().tool_use(&tool_use).call().unwrap();

        bash.handle_event(&Event::Answer(AnswerEvent::ToolResult {
            id: "tool_1".to_string(),
            is_error: false,
            is_user_cancelled: false,
            output: Final::Json(value),
        }));
        assert_eq!(bash.state.display_state, FoldState::Preview);
        assert_eq!(bash.height(80), bash.input.height(80));

        bash.update(&Action::Blur);
        assert_eq!(bash.state.display_state, FoldState::Preview);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bash_empty_output_summary_is_available() {
        let tool_use = tool_use();
        let value = serde_json::to_value(bash_output_empty()).unwrap();
        let bash = Bash::try_new()
            .tool_use(&tool_use)
            .output(value)
            .call()
            .unwrap();

        assert_eq!(
            bash.empty_output_summary(),
            Some("Exit 0 (no output)".to_string())
        );
    }
}
