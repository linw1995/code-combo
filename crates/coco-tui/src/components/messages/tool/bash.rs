use bon::bon;
use coco_macro::{ComponentExt, ContentComponentExt};
use code_combo::{
    ToolUse,
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
    #[serde(default)]
    view: BashOutputView,
    #[serde(default)]
    collapsed: bool,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "bash")]
pub struct Bash<'a> {
    state: State<Inner>,

    input: CodeHighlight<'a>,
    output_text: Paragraph<'a>,
    output_markers: Option<Paragraph<'a>>,
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
    let highlight = Style::default().reversed();
    let items = [
        (BashOutputView::Stdout, " 1 ", Style::default().blue()),
        (BashOutputView::Stderr, " 2 ", Style::default().red()),
        (BashOutputView::Mixed, " 3 ", Style::default().dark_gray()),
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

fn generate_output<'a>(output: Option<&BashOutput>, view: BashOutputView) -> Paragraph<'a> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    let Some(output) = output else {
        return Paragraph::new_wrap(lines, Wrap { trim: false });
    };

    match view {
        BashOutputView::Stdout => {
            for line in output.stdout.lines() {
                lines.push(Line::from(line.to_string()));
            }
            Paragraph::new_wrap(lines, Wrap { trim: false })
        }
        BashOutputView::Stderr => {
            for line in output.stderr.lines() {
                lines.push(Line::from(line.to_string()));
            }
            Paragraph::new_wrap(lines, Wrap { trim: false })
        }
        BashOutputView::Mixed => {
            if output.chunks.is_empty() {
                for text in [&output.stderr, &output.stdout] {
                    if text.is_empty() {
                        continue;
                    }
                    for line in text.lines() {
                        lines.push(Line::from(line.to_string()));
                    }
                }
            } else {
                for chunk in &output.chunks {
                    for line in &chunk.lines {
                        lines.push(Line::from(line.clone()));
                    }
                }
            }
            Paragraph::new(lines)
        }
    }
}

fn generate_output_markers<'a>(
    output: Option<&BashOutput>,
    view: BashOutputView,
) -> Option<Paragraph<'a>> {
    if view != BashOutputView::Mixed {
        return None;
    }
    let Some(output) = output else {
        return Some(Paragraph::new(Vec::<Line>::new()));
    };

    let mut lines: Vec<Line<'a>> = Vec::new();
    if output.chunks.is_empty() {
        for (marker_style, text) in [
            (Style::default().red(), &output.stderr),
            (Style::default().blue(), &output.stdout),
        ] {
            if text.is_empty() {
                continue;
            }
            for _ in text.lines() {
                lines.push(Line::from(Span::styled("▌", marker_style)));
            }
        }
    } else {
        for chunk in &output.chunks {
            let marker_style = match chunk.stream {
                code_combo::StreamKind::Stdout => Style::default().blue(),
                code_combo::StreamKind::Stderr => Style::default().red(),
            };
            for _ in &chunk.lines {
                lines.push(Line::from(Span::styled("▌", marker_style)));
            }
        }
    }

    Some(Paragraph::new(lines))
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
        let output = output
            .map(serde_json::from_value)
            .transpose()
            .whatever_context("failed to parse BashOutput")?;
        let output_text = generate_output(output.as_ref(), BashOutputView::default());
        let output_markers = generate_output_markers(output.as_ref(), BashOutputView::default());

        Ok(Self {
            state: State::new(Inner {
                tool_use: tool_use.to_owned(),
                requiring_confirmation: false,
                output,
                view: BashOutputView::default(),
                collapsed: false,
            }),
            input,
            output_text,
            output_markers,
        })
    }

    fn rebuild_output(&mut self) {
        self.output_text = generate_output(self.state.output.as_ref(), self.state.view);
        self.output_markers = generate_output_markers(self.state.output.as_ref(), self.state.view);
    }

    pub fn update_output(&mut self, output: Option<Final>) -> Result<()> {
        if let Some(Final::Json(value)) = output {
            let output =
                serde_json::from_value(value).whatever_context("failed to parse BashOutput")?;
            self.state.write().output = Some(output);
            self.rebuild_output();
        }
        Ok(())
    }

    fn has_output_content(&self) -> bool {
        match self.state.output.as_ref() {
            Some(output) => {
                !(output.stdout.is_empty() && output.stderr.is_empty() && output.chunks.is_empty())
            }
            None => false,
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
        if self.state.requiring_confirmation || self.state.collapsed || !self.has_output_content() {
            return self.input.height(width);
        }
        let height_input = self.input.height(width);
        let tab_width = tab_panel_width(width);
        let body_width = width.saturating_sub(tab_width).max(1);
        let marker_width = output_marker_width(self.state.view);
        let text_width = body_width.saturating_sub(marker_width).max(1);
        let height_output = self.output_text.line_count(text_width).max(1);
        height_input + height_output
    }

    fn is_actionable(&self) -> bool {
        true
    }

    fn block_with_shortcuts_desc<'b>(&self, block: Block<'b>) -> Block<'b> {
        if self.state.requiring_confirmation {
            return block
                .title_bottom(shortcuts_desc(&[("Run", "CR")]))
                .title_bottom(shortcuts_desc(&[("Cancel", "Esc")]));
        }

        if !self.has_output_content() {
            return block;
        }

        let toggle_text = if self.state.collapsed {
            ("Unfold", "z")
        } else {
            ("Fold", "z")
        };

        let view = match self.state.view {
            BashOutputView::Stdout => "Stdout",
            BashOutputView::Stderr => "Stderr",
            BashOutputView::Mixed => "Mixed",
        };
        block
            .title_bottom(shortcuts_desc(&[(view, "1/2/3")]))
            .title_bottom(shortcuts_desc(&[toggle_text]))
    }

    fn reminder_line(&self) -> Option<Line<'static>> {
        let mut spans = Vec::new();
        if let Some(summary) = self.empty_output_summary() {
            spans.push(Span::raw(format!(" - {summary}")).dark_gray());
        }
        if self.state.collapsed && self.has_output_content() {
            spans.push(Span::raw(" (folded)").dark_gray());
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
        Ok(Self {
            input: generate_input(&state.tool_use),
            output_text: generate_output(state.output.as_ref(), state.view),
            output_markers: generate_output_markers(state.output.as_ref(), state.view),
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
                state.collapsed = false;
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
                    chunks: Vec::new(),
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
                output.chunks.push(chunk.clone());
                drop(state);
                self.rebuild_output();
            }
            Event::Answer(AnswerEvent::ToolResult { output, .. }) => {
                if let Err(err) = self.update_output(Some(output.to_owned())) {
                    warn!(?err, "failed to update tool output");
                };
                let mut state = self.state.write();
                state.collapsed = false;
                state.requiring_confirmation = false;
            }
            _ => (),
        }
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('1')) => {
                if !self.has_output_content() {
                    return;
                }
                let mut state = self.state.write();
                state.view = BashOutputView::Stdout;
                state.collapsed = false;
                drop(state);
                self.rebuild_output();
            }
            (KeyModifiers::NONE, KeyCode::Char('2')) => {
                if !self.has_output_content() {
                    return;
                }
                let mut state = self.state.write();
                state.view = BashOutputView::Stderr;
                state.collapsed = false;
                drop(state);
                self.rebuild_output();
            }
            (KeyModifiers::NONE, KeyCode::Char('3')) => {
                if !self.has_output_content() {
                    return;
                }
                let mut state = self.state.write();
                state.view = BashOutputView::Mixed;
                state.collapsed = false;
                drop(state);
                self.rebuild_output();
            }
            (KeyModifiers::NONE, KeyCode::Char('z')) => {
                if !self.has_output_content() {
                    return;
                }
                self.state.write().collapsed = !self.state.collapsed;
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if !self.state.requiring_confirmation {
                    return;
                }
                self.state.write().collapsed = false;
                global::action_tx()
                    .send(ToolAction::Grant(self.state.tool_use.to_owned()).into())
                    .unwrap();
                self.state.write().requiring_confirmation = false;
            }
            (KeyModifiers::NONE, KeyCode::Esc) => {
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
        let Some(output) = self.state.output.as_ref() else {
            return;
        };
        if output.exit_code != 0 {
            return;
        }
        self.state.write().collapsed = true;
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        if area.height == 0 {
            return Ok(());
        }

        use Constraint::Length;

        let width = area.width.max(1);
        let height_input = self.input.height(width);

        if self.state.collapsed || self.state.requiring_confirmation || !self.has_output_content() {
            let [area_input] = Layout::vertical([Length(height_input as u16)]).areas(area);
            self.input.draw(frame, area_input)?;
            return Ok(());
        }

        let tab_width = tab_panel_width(width);
        let marker_width = output_marker_width(self.state.view);
        let body_width = width.saturating_sub(tab_width).max(1);
        let text_width = body_width.saturating_sub(marker_width).max(1);
        let height_output = self.output_text.line_count(text_width).max(1);
        let [area_input, area_output] =
            Layout::vertical([Length(height_input as u16), Length(height_output as u16)])
                .areas(area);

        self.input.draw(frame, area_input)?;

        let (area_output_view, area_output_tabs) = if tab_width == 0 {
            (area_output, None)
        } else {
            let [view, tabs] =
                Layout::horizontal([Constraint::Min(1), Constraint::Length(tab_width)])
                    .areas(area_output);
            (view, Some(tabs))
        };

        if marker_width == 0 {
            frame.render_widget(&self.output_text, area_output_view);
        } else {
            let [area_text, area_markers] =
                Layout::horizontal([Constraint::Min(1), Constraint::Length(marker_width)])
                    .areas(area_output_view);
            frame.render_widget(&self.output_text, area_text);
            if let Some(markers) = &self.output_markers {
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
            chunks: vec![
                code_combo::OutputChunk {
                    timestamp: 0,
                    stream: code_combo::StreamKind::Stdout,
                    lines: vec!["out".to_string()],
                },
                code_combo::OutputChunk {
                    timestamp: 0,
                    stream: code_combo::StreamKind::Stderr,
                    lines: vec!["err".to_string()],
                },
            ],
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
            chunks: Vec::new(),
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
        assert!(!bash.state.collapsed);

        let value = serde_json::to_value(bash_output()).unwrap();
        bash.handle_event(&Event::Answer(AnswerEvent::ToolResult {
            id: "tool_1".to_string(),
            is_error: false,
            output: Final::Json(value),
        }));
        assert!(!bash.state.collapsed);
        bash.update(&Action::Blur);
        assert!(bash.state.collapsed);

        let value = serde_json::to_value(bash_output_with_exit(1)).unwrap();
        bash.handle_event(&Event::Answer(AnswerEvent::ToolResult {
            id: "tool_1".to_string(),
            is_error: true,
            output: Final::Json(value),
        }));
        assert!(!bash.state.collapsed);
        bash.update(&Action::Blur);
        assert!(!bash.state.collapsed);
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

        bash.state.write().collapsed = true;
        bash.handle_key_event(&key(KeyCode::Char('1')));
        assert!(!bash.state.collapsed);
        assert_eq!(bash.state.view.index(), 0);

        bash.state.write().collapsed = true;
        bash.handle_key_event(&key(KeyCode::Char('2')));
        assert!(!bash.state.collapsed);
        assert_eq!(bash.state.view.index(), 1);

        bash.state.write().collapsed = true;
        bash.handle_key_event(&key(KeyCode::Char('3')));
        assert!(!bash.state.collapsed);
        assert_eq!(bash.state.view.index(), 2);
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
        assert_eq!(output.chunks.len(), 1);
        assert_eq!(
            output.chunks[0].lines,
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

        bash.state.write().collapsed = true;
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
            output: Final::Json(value),
        }));
        assert!(!bash.state.collapsed);
        assert_eq!(bash.height(80), bash.input.height(80));

        bash.update(&Action::Blur);
        assert!(!bash.state.collapsed);
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
