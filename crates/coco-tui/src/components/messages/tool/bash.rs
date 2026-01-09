use std::ops::Range;

use bon::bon;
use coco_highlight::Lang;
use coco_macro::{ComponentExt, ContentComponentExt};
use code_combo::{
    OutputChunk, ToolUse, bash_unsafe_ranges,
    tools::{BashInput, BashOutput, Final},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
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
    actions::{Action, ToolAction},
    components::{CodeHighlight, Persistable, ShortcutHints},
    error::*,
    events::{AnswerEvent, AskEvent, Event},
    global::{self, State},
    session::{self, Session},
    widgets::Paragraph,
};

#[derive(Serialize, Deserialize)]
struct Inner {
    tool_use: ToolUse,
    exec_state: ExecState,
    #[serde(default)]
    display_state: FoldState,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "bash")]
pub struct Bash<'a> {
    state: State<Inner>,

    input: CodeHighlight<'a>,

    preview_lines: StreamedLines,
    output_text: Paragraph<'a>,
    output_markers: Option<Paragraph<'a>>,
    theme_dirty: bool,
    is_focused: bool,
    defer_auto_collapse: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExecState {
    Initial {
        requiring_confirmation: bool,
    },
    Executing {
        chunks: Vec<OutputChunk>,
    },
    Finished {
        output: BashOutput,
        chunks: Vec<OutputChunk>,
        view: BashOutputView,
    },
}

impl Default for ExecState {
    fn default() -> Self {
        Self::Initial {
            requiring_confirmation: false,
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

const OUTPUT_MARKER: &str = "▐";

fn build_input<'b>(command: &str, unsafe_ranges: &[Range<usize>]) -> CodeHighlight<'b> {
    if unsafe_ranges.is_empty() {
        CodeHighlight::try_new(command, Lang::Bash).expect("failed to new CodeHighlight")
    } else {
        CodeHighlight::try_new_with_ranges(command, Lang::Bash, unsafe_ranges.to_vec())
            .expect("failed to new CodeHighlight")
    }
}

fn generate_input<'b>(tool_use: &ToolUse, exec_state: &ExecState) -> CodeHighlight<'b> {
    let input: BashInput =
        serde_json::from_value(tool_use.input.clone()).expect("failed to parse BashInput");
    let unsafe_ranges: Vec<Range<usize>> = match exec_state {
        ExecState::Initial {
            requiring_confirmation: true,
        } => bash_unsafe_ranges(&input.command)
            .into_iter()
            .map(|(range, _)| range)
            .collect(),
        _ => Vec::new(),
    };
    build_input(&input.command, &unsafe_ranges)
}

#[bon]
impl<'a> Bash<'a> {
    #[builder]
    pub fn try_new(tool_use: &ToolUse, output: Option<Value>) -> Result<Self> {
        let output: Option<BashOutput> = output
            .map(serde_json::from_value)
            .transpose()
            .whatever_context("failed to parse BashOutput")?;
        let display_state = display_state_for_output(output.as_ref(), FoldState::Preview);
        let exec_state = match output {
            Some(output) => ExecState::Finished {
                output,
                chunks: Vec::new(),
                view: BashOutputView::default(),
            },
            None => ExecState::Initial {
                requiring_confirmation: false,
            },
        };
        let input = generate_input(tool_use, &exec_state);
        let preview_lines = StreamedLines::new(Some(OUTPUT_PREVIEW_LINES));
        let mut component = Self {
            state: State::new(Inner {
                tool_use: tool_use.to_owned(),
                exec_state,
                display_state,
            }),
            input,
            preview_lines,
            output_text: Paragraph::new(Vec::new()),
            output_markers: None,
            theme_dirty: false,
            is_focused: false,
            defer_auto_collapse: false,
        };
        component.rebuild_output();
        Ok(component)
    }

    fn render_output(&self) -> (Paragraph<'a>, Option<Paragraph<'a>>) {
        let theme = global::theme();
        let empty = || {
            (
                Paragraph::new_wrap(Vec::new(), Wrap { trim: false }),
                Some(Paragraph::new(Vec::<Line>::new())),
            )
        };

        match &self.state.exec_state {
            ExecState::Initial { .. } => empty(),
            ExecState::Executing { chunks } => {
                if self.preview_lines.is_empty() && chunks.is_empty() {
                    return empty();
                }
                let mut lines: Vec<Line<'a>> = Vec::new();
                let mut markers: Vec<Line<'a>> = Vec::new();
                for line in self.preview_lines.iter() {
                    let marker_style = match line.stream {
                        code_combo::StreamKind::Stdout => theme.ui.bash_stdout_marker,
                        code_combo::StreamKind::Stderr => theme.ui.bash_stderr_marker,
                    };
                    lines.push(Line::from(line.text.clone()));
                    markers.push(Line::from(Span::styled(OUTPUT_MARKER, marker_style)));
                }
                (Paragraph::new(lines), Some(Paragraph::new(markers)))
            }
            ExecState::Finished {
                output,
                chunks,
                view,
            } => match view {
                BashOutputView::Stdout => {
                    let mut lines: Vec<Line<'a>> = Vec::new();
                    for line in output.stdout.lines() {
                        lines.push(Line::from(line.to_string()));
                    }
                    (Paragraph::new_wrap(lines, Wrap { trim: false }), None)
                }
                BashOutputView::Stderr => {
                    let mut lines: Vec<Line<'a>> = Vec::new();
                    for line in output.stderr.lines() {
                        lines.push(Line::from(line.to_string()));
                    }
                    (Paragraph::new_wrap(lines, Wrap { trim: false }), None)
                }
                BashOutputView::Mixed => {
                    let mut lines: Vec<Line<'a>> = Vec::new();
                    let mut markers: Vec<Line<'a>> = Vec::new();
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
                    (Paragraph::new(lines), Some(Paragraph::new(markers)))
                }
            },
        }
    }

    fn rebuild_output(&mut self) {
        let (output_text, output_markers) = self.render_output();
        self.output_text = output_text;
        self.output_markers = output_markers;
        self.theme_dirty = false;
    }

    fn rebuild_input(&mut self) {
        let state = self.state.read();
        self.input = generate_input(&state.tool_use, &state.exec_state);
    }

    fn exec_output(&self) -> Option<&BashOutput> {
        match &self.state.exec_state {
            ExecState::Finished { output, .. } => Some(output),
            _ => None,
        }
    }

    fn exec_chunks(&self) -> &[OutputChunk] {
        match &self.state.exec_state {
            ExecState::Executing { chunks } | ExecState::Finished { chunks, .. } => chunks,
            ExecState::Initial { .. } => &[],
        }
    }

    fn exec_view(&self) -> BashOutputView {
        match &self.state.exec_state {
            ExecState::Finished { view, .. } => *view,
            _ => BashOutputView::Mixed,
        }
    }

    fn set_exec_view(&mut self, view: BashOutputView) -> bool {
        let mut state = self.state.write();
        if let ExecState::Finished { view: current, .. } = &mut state.exec_state {
            *current = view;
            return true;
        }
        false
    }

    fn requiring_confirmation(&self) -> bool {
        matches!(
            &self.state.exec_state,
            ExecState::Initial {
                requiring_confirmation: true
            }
        )
    }

    fn set_requiring_confirmation(&mut self, value: bool) {
        {
            let mut state = self.state.write();
            if let ExecState::Initial {
                requiring_confirmation,
            } = &mut state.exec_state
            {
                *requiring_confirmation = value;
            }
        }
        self.rebuild_input();
    }

    fn push_chunk(&mut self, chunk: OutputChunk) {
        let chunk_for_state = chunk.clone();
        let mut state = self.state.write();
        match &mut state.exec_state {
            ExecState::Executing { chunks } => chunks.push(chunk_for_state),
            ExecState::Finished { chunks, .. } => chunks.push(chunk_for_state),
            ExecState::Initial { .. } => {
                state.exec_state = ExecState::Executing {
                    chunks: vec![chunk_for_state],
                };
            }
        }
        self.preview_lines.push_chunk(&chunk);
    }

    pub fn update_output(&mut self, output: Option<Final>) -> Result<()> {
        if let Some(Final::Json(value)) = output {
            let output = serde_json::from_value::<BashOutput>(value)
                .whatever_context("failed to parse BashOutput")?;
            {
                let mut state = self.state.write();
                let (chunks, view) = match &mut state.exec_state {
                    ExecState::Executing { chunks } => {
                        (std::mem::take(chunks), BashOutputView::default())
                    }
                    ExecState::Finished { chunks, view, .. } => (std::mem::take(chunks), *view),
                    ExecState::Initial { .. } => (Vec::new(), BashOutputView::default()),
                };
                state.exec_state = ExecState::Finished {
                    output,
                    chunks,
                    view,
                };
            }
            self.preview_lines =
                StreamedLines::from_chunks(self.exec_chunks(), Some(OUTPUT_PREVIEW_LINES));
            self.rebuild_output();
        }
        Ok(())
    }

    fn has_output_content(&self) -> bool {
        let output = self.exec_output();
        let has_text =
            output.is_some_and(|output| !(output.stdout.is_empty() && output.stderr.is_empty()));
        has_text || !self.exec_chunks().is_empty() || !self.preview_lines.is_empty()
    }

    pub fn empty_output_summary(&self) -> Option<String> {
        if self.has_output_content() {
            return None;
        }
        let output = self.exec_output()?;
        if output.timed_out {
            return Some(format!("Timed out (exit {}, no output)", output.exit_code));
        }
        Some(format!("Exit {} (no output)", output.exit_code))
    }
}

impl<'a> Content for Bash<'a> {
    fn height(&self, width: u16) -> usize {
        if self.requiring_confirmation()
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
        let width_marker = output_marker_width(self.exec_view());
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

    fn shortcut_hints(&self) -> ShortcutHints {
        if self.requiring_confirmation() {
            let mut hints = ShortcutHints::default();
            hints.push_visible(&[("Run", "CR"), ("Allow in Session", "A")]);
            hints.push_visible(&[("Cancel", "Esc")]);
            return hints;
        }

        if !self.has_output_content() {
            return ShortcutHints::default();
        }

        let toggle_text = match self.state.display_state {
            FoldState::Expanded => ("Fold", "z"),
            FoldState::Preview | FoldState::Collapsed => ("Expand", "z"),
        };

        let mut hints = ShortcutHints::from_visible(&[toggle_text]);
        if matches!(self.state.display_state, FoldState::Expanded) {
            let view = match self.exec_view() {
                BashOutputView::Stdout => "Stdout",
                BashOutputView::Stderr => "Stderr",
                BashOutputView::Mixed => "Mixed",
            };
            hints.push_hidden(&[(view, "1/2/3")]);
        }

        hints
    }

    fn reminder_line(&self) -> Option<Line<'static>> {
        let mut spans = Vec::new();
        let theme = global::theme();
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

impl Persistable for Bash<'static> {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: Inner = session::load(session)?;
        let streamed_preview_lines = match &state.exec_state {
            ExecState::Executing { chunks } | ExecState::Finished { chunks, .. } => {
                StreamedLines::from_chunks(chunks, Some(OUTPUT_PREVIEW_LINES))
            }
            ExecState::Initial { .. } => StreamedLines::new(Some(OUTPUT_PREVIEW_LINES)),
        };
        let mut component = Self {
            input: generate_input(&state.tool_use, &state.exec_state),
            preview_lines: streamed_preview_lines,
            output_text: Paragraph::new(Vec::new()),
            output_markers: None,
            state: global::State::new(state),
            theme_dirty: false,
            is_focused: false,
            defer_auto_collapse: false,
        };
        component.rebuild_output();
        Ok(component)
    }
}

impl Component for Bash<'static> {
    fn on_cache_invalidation(&mut self, reason: CacheInvalidation) {
        if matches!(reason, CacheInvalidation::Theme) {
            self.theme_dirty = true;
            self.input.invalidate_cache(reason);
        }
    }

    fn handle_event(&mut self, event: &Event) {
        match event {
            Event::Ask(AskEvent::ToolUsePermission(_)) => {
                self.set_requiring_confirmation(true);
                self.state.write().display_state.preview();
            }
            Event::Answer(AnswerEvent::ToolOutput { id, chunk }) => {
                if id != &self.state.tool_use.id {
                    return;
                }
                self.push_chunk(chunk.clone());
                self.rebuild_output();
            }
            Event::Answer(AnswerEvent::ToolResult { output, .. }) => {
                if let Err(err) = self.update_output(Some(output.to_owned())) {
                    warn!(?err, "failed to update tool output");
                };
                let display_state =
                    display_state_for_output(self.exec_output(), self.state.display_state);
                {
                    let mut state = self.state.write();
                    state.display_state = display_state;
                }
                self.defer_auto_collapse = self.is_focused
                    && matches!(display_state, FoldState::Preview)
                    && self
                        .exec_output()
                        .is_some_and(|output| output.exit_code == 0)
                    && self.has_output_content();
                self.set_requiring_confirmation(false);
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
                if !self.set_exec_view(BashOutputView::Stdout) {
                    return;
                }
                self.rebuild_output();
            }
            (FoldState::Expanded, KeyModifiers::NONE, KeyCode::Char('2')) => {
                if !self.has_output_content() {
                    return;
                }
                if !self.set_exec_view(BashOutputView::Stderr) {
                    return;
                }
                self.state.write().display_state.expand();
                self.rebuild_output();
            }
            (FoldState::Expanded, KeyModifiers::NONE, KeyCode::Char('3')) => {
                if !self.has_output_content() {
                    return;
                }
                if !self.set_exec_view(BashOutputView::Mixed) {
                    return;
                }
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
                if !self.requiring_confirmation() {
                    return;
                }
                self.state.write().display_state.preview();
                global::action_tx()
                    .send(ToolAction::Grant(self.state.tool_use.to_owned()).into())
                    .unwrap();
                self.set_requiring_confirmation(false);
            }
            (_, KeyModifiers::NONE, KeyCode::Char('a') | KeyCode::Char('A')) => {
                if !self.requiring_confirmation() {
                    return;
                }
                self.state.write().display_state.preview();
                global::action_tx()
                    .send(ToolAction::GrantSession(self.state.tool_use.to_owned()).into())
                    .unwrap();
                self.set_requiring_confirmation(false);
            }
            (_, KeyModifiers::NONE, KeyCode::Esc) => {
                if !self.requiring_confirmation() {
                    return;
                }
                global::action_tx()
                    .send(ToolAction::Cancel(self.state.tool_use.to_owned()).into())
                    .unwrap();
                self.set_requiring_confirmation(false);
            }
            _ => (), // ignore
        }
    }

    fn update(&mut self, action: &Action) {
        match action {
            Action::Focus => {
                self.is_focused = true;
            }
            Action::Blur => {
                self.is_focused = false;
                if self.defer_auto_collapse {
                    self.defer_auto_collapse = false;
                    return;
                }
                if self.requiring_confirmation() {
                    return;
                }
                if !self.has_output_content() {
                    return;
                }
                if self.state.display_state != FoldState::Preview {
                    return;
                }
                let Some(output) = self.exec_output() else {
                    return;
                };
                if output.exit_code != 0 {
                    return;
                }
                self.state.write().display_state.collapse();
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

        let width = area.width.max(1);
        let height_input = self.input.height(width);

        if self.state.display_state == FoldState::Collapsed
            || self.requiring_confirmation()
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
        let width_marker = output_marker_width(self.exec_view());
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

        if width_marker == 0 {
            frame.render_widget(&self.output_text, area_output_view);
        } else {
            let [area_text, area_markers] =
                Layout::horizontal([Constraint::Min(1), Constraint::Length(width_marker)])
                    .areas(area_output_view);
            frame.render_widget(&self.output_text, area_text);
            if let Some(markers) = &self.output_markers {
                frame.render_widget(markers, area_markers);
            }
        }

        if let Some(area_tabs) = area_output_tabs {
            let tabs_panel = render_tabs_panel(self.exec_view());
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
        assert!(matches!(&bash.state.exec_state, ExecState::Finished { .. }));
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
    async fn bash_delays_auto_collapse_until_next_blur_when_focused() {
        let tool_use = tool_use();
        let mut bash = Bash::try_new().tool_use(&tool_use).call().unwrap();
        bash.update(&Action::Focus);

        let value = serde_json::to_value(bash_output()).unwrap();
        bash.handle_event(&Event::Answer(AnswerEvent::ToolResult {
            id: "tool_1".to_string(),
            is_error: false,
            is_user_cancelled: false,
            output: Final::Json(value),
        }));
        assert_eq!(bash.state.display_state, FoldState::Preview);

        bash.update(&Action::Blur);
        assert_eq!(bash.state.display_state, FoldState::Preview);

        bash.update(&Action::Blur);
        assert_eq!(bash.state.display_state, FoldState::Collapsed);
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
        assert_eq!(bash.exec_view().index(), 0);

        bash.handle_key_event(&key(KeyCode::Char('2')));
        assert_eq!(bash.state.display_state, FoldState::Expanded);
        assert_eq!(bash.exec_view().index(), 1);

        bash.handle_key_event(&key(KeyCode::Char('3')));
        assert_eq!(bash.state.display_state, FoldState::Expanded);
        assert_eq!(bash.exec_view().index(), 2);
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

        assert!(matches!(
            &bash.state.exec_state,
            ExecState::Executing { .. }
        ));
        let chunks = match &bash.state.exec_state {
            ExecState::Executing { chunks } => chunks,
            _ => panic!("expected executing state"),
        };
        assert_eq!(chunks.len(), 1);
        assert_eq!(
            chunks[0].lines,
            vec!["out1".to_string(), "out2".to_string()]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bash_hides_tabs_while_requiring_confirmation() {
        let tool_use = tool_use();
        let mut bash = Bash::try_new().tool_use(&tool_use).call().unwrap();
        assert!(!bash.requiring_confirmation());
        bash.handle_event(&Event::Ask(AskEvent::ToolUsePermission(
            "tool_1".to_string(),
        )));
        assert!(bash.requiring_confirmation());
        assert_eq!(bash.height(80), bash.input.height(80));
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
