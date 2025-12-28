use coco_macro::{ComponentExt, ContentComponentExt};
use code_combo::{
    TextEdit, ToolUse,
    tools::{Final, STR_REPLACE_TOOL_NAME, StrReplaceInput},
};
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

const DEFAULT_CONTEXT_RADIUS: usize = 3;
const MIN_CONTEXT_RADIUS: usize = 0;
const MAX_CONTEXT_RADIUS: usize = 10;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
enum DisplayState {
    #[default]
    Preview,
    Result,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
enum ResultView {
    #[default]
    Applied,
    Unapplied,
}

#[derive(Clone, Serialize, Deserialize)]
struct Inner {
    tool_use: ToolUse,
    edit: Option<TextEdit>,
    #[serde(default = "default_context_radius")]
    context_radius: usize,
    #[serde(default)]
    hunk_idx: usize,
    #[serde(default)]
    display_state: DisplayState,
    #[serde(default)]
    collapsed: bool,
    #[serde(default)]
    result_view: ResultView,
    #[serde(default)]
    applied_diffs: Vec<String>,
    #[serde(default)]
    rejected_diffs: Vec<String>,
    #[serde(default)]
    pending_apply_diff: Option<String>,
    #[serde(default)]
    auto_accept_pending: bool,
    #[serde(default)]
    result_message: Option<String>,
    #[serde(default)]
    result_is_error: Option<bool>,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "str_replace")]
pub struct StrReplace<'a> {
    state: State<Inner>,
    header: Paragraph<'a>,
    widget: StrReplaceWidget<'a>,
}

enum StrReplaceWidget<'a> {
    CodeHighlight(CodeHighlight<'a>),
    Paragraph(Paragraph<'a>),
    Empty,
}

impl StrReplaceWidget<'_> {
    fn height(&self, width: u16) -> usize {
        match self {
            Self::CodeHighlight(highlight) => highlight.height(width),
            Self::Paragraph(paragraph) => paragraph.line_count(width),
            Self::Empty => 0,
        }
    }
}

fn default_context_radius() -> usize {
    DEFAULT_CONTEXT_RADIUS
}

fn tool_path(tool_use: &ToolUse) -> Option<String> {
    let input: StrReplaceInput = serde_json::from_value(tool_use.input.clone()).ok()?;
    Some(input.path)
}

fn hunk_total(edit: Option<&TextEdit>, context_radius: usize) -> usize {
    let Some(edit) = edit else {
        return 0;
    };
    edit.text_diff()
        .unified_diff()
        .context_radius(context_radius)
        .iter_hunks()
        .count()
}

fn clamp_hunk_idx(state: &mut Inner) {
    let total = hunk_total(state.edit.as_ref(), state.context_radius);
    if total == 0 {
        state.hunk_idx = 0;
        return;
    }
    if state.hunk_idx >= total {
        state.hunk_idx = total - 1;
    }
}

fn effective_hunk_idx(state: &Inner) -> usize {
    let total = hunk_total(state.edit.as_ref(), state.context_radius);
    if total == 0 {
        0
    } else {
        state.hunk_idx.min(total - 1)
    }
}

fn build_hunk_diff(edit: &TextEdit, context_radius: usize, hunk_idx: usize) -> Option<String> {
    let diff = edit.text_diff();
    let hunk = diff
        .unified_diff()
        .context_radius(context_radius)
        .iter_hunks()
        .nth(hunk_idx)?;

    let mut buf = vec![];
    if let Err(e) = hunk.to_writer(&mut buf) {
        warn!(error = ?e, "failed to write unified diff into memory");
        return None;
    }
    Some(String::from_utf8_lossy(&buf).to_string())
}

fn join_diffs(diffs: &[String]) -> String {
    diffs.join("\n")
}

fn build_diff_widget(diff_text: String) -> StrReplaceWidget<'static> {
    match CodeHighlight::try_new(&diff_text, code_highlight::Lang::Diff) {
        Ok(highlight) => StrReplaceWidget::CodeHighlight(highlight),
        Err(_) => StrReplaceWidget::Paragraph(Paragraph::new(diff_text)),
    }
}

fn tab_panel_width(total_width: u16) -> u16 {
    let min_total_width = 24u16;
    if total_width < min_total_width {
        return 0;
    }
    3
}

fn render_tabs_panel(view: ResultView) -> Paragraph<'static> {
    let highlight = Style::default().reversed();
    let items = [
        (ResultView::Applied, " 1 ", Style::default().green()),
        (ResultView::Unapplied, " 2 ", Style::default().red()),
    ];
    let lines = items
        .into_iter()
        .map(|(v, digit, base_style)| {
            let style = if v == view {
                highlight
            } else {
                Style::default()
            };
            Line::from(Span::styled(digit, base_style.patch(style)))
        })
        .collect::<Vec<_>>();
    Paragraph::new(lines)
}

fn has_result_tabs(state: &Inner) -> bool {
    !state.applied_diffs.is_empty() && !state.rejected_diffs.is_empty()
}

fn has_result_content(state: &Inner) -> bool {
    !state.applied_diffs.is_empty()
        || !state.rejected_diffs.is_empty()
        || state.result_message.is_some()
}

fn build_header(state: &Inner) -> Paragraph<'static> {
    let mut lines = Vec::new();
    let path = tool_path(&state.tool_use).unwrap_or_else(|| "unknown".to_string());
    lines.push(Line::from(vec![
        Span::styled(" File: ", Style::default().blue()),
        Span::raw(path),
    ]));

    match state.display_state {
        DisplayState::Preview => {
            if state.edit.is_some() {
                let total = hunk_total(state.edit.as_ref(), state.context_radius);
                let current = if total == 0 {
                    0
                } else {
                    state.hunk_idx.min(total - 1) + 1
                };
                lines.push(Line::from(vec![
                    Span::styled(" Hunk: ", Style::default().blue()),
                    Span::raw(format!("{current}/{total}")),
                ]));
                lines.push(Line::from(vec![
                    Span::styled(" Context: ", Style::default().blue()),
                    Span::raw(state.context_radius.to_string()),
                ]));
            }
        }
        DisplayState::Result => {
            if let Some(message) = &state.result_message {
                let style = if state.result_is_error.unwrap_or(false) {
                    Style::default().red()
                } else {
                    Style::default().green()
                };
                lines.push(Line::from(vec![
                    Span::styled(" Result: ", Style::default().blue()),
                    Span::styled(message.clone(), style),
                ]));
            }
        }
    }

    Paragraph::new_wrap(lines, Wrap { trim: false })
}

fn build_preview_widget(state: &Inner) -> StrReplaceWidget<'static> {
    let Some(edit) = state.edit.as_ref() else {
        return StrReplaceWidget::Empty;
    };

    let total = hunk_total(Some(edit), state.context_radius);
    if total == 0 {
        return StrReplaceWidget::Paragraph(Paragraph::new("No diffs"));
    }

    let idx = state.hunk_idx.min(total - 1);
    let diff_text = build_hunk_diff(edit, state.context_radius, idx).unwrap_or_default();

    if diff_text.is_empty() {
        StrReplaceWidget::Paragraph(Paragraph::new("No diffs"))
    } else {
        build_diff_widget(diff_text)
    }
}

fn build_result_widget(state: &Inner) -> StrReplaceWidget<'static> {
    // If there's no tab view (only one type of diff), show all available diffs
    if !has_result_tabs(state) {
        let diffs = if !state.applied_diffs.is_empty() {
            &state.applied_diffs
        } else {
            &state.rejected_diffs
        };
        let diff_text = join_diffs(diffs);

        if diff_text.is_empty() {
            StrReplaceWidget::Paragraph(Paragraph::new("No diffs"))
        } else {
            build_diff_widget(diff_text)
        }
    } else {
        // Has tab view, display based on current selected view
        let diffs = match state.result_view {
            ResultView::Applied => &state.applied_diffs,
            ResultView::Unapplied => &state.rejected_diffs,
        };
        let diff_text = join_diffs(diffs);

        if diff_text.is_empty() {
            StrReplaceWidget::Paragraph(Paragraph::new("No diffs"))
        } else {
            build_diff_widget(diff_text)
        }
    }
}

impl<'a> StrReplace<'a> {
    pub fn new(tool_use: &ToolUse) -> Self {
        let mut inst = Self {
            state: State::new(Inner {
                tool_use: tool_use.to_owned(),
                edit: None,
                context_radius: DEFAULT_CONTEXT_RADIUS,
                hunk_idx: 0,
                display_state: DisplayState::default(),
                collapsed: false,
                result_view: ResultView::default(),
                applied_diffs: Vec::new(),
                rejected_diffs: Vec::new(),
                pending_apply_diff: None,
                auto_accept_pending: false,
                result_message: None,
                result_is_error: None,
            }),
            header: Paragraph::new(""),
            widget: StrReplaceWidget::Empty,
        };
        inst.rebuild_view();
        inst
    }

    fn rebuild_view(&mut self) {
        let state = self.state.get();
        self.header = build_header(&state);
        self.widget = match state.display_state {
            DisplayState::Preview => build_preview_widget(&state),
            DisplayState::Result => build_result_widget(&state),
        };
    }

    pub fn update_text_edit(&mut self, edit: TextEdit) {
        let mut state = self.state.write();
        state.edit = Some(edit);
        state.display_state = DisplayState::Preview;
        state.collapsed = false;
        state.auto_accept_pending = false;
        clamp_hunk_idx(&mut state);
        drop(state);
        self.rebuild_view();
    }

    fn diff_height(&self, width: u16) -> usize {
        let width = width.max(1);
        let state = self.state.read();
        if state.display_state == DisplayState::Result && state.collapsed {
            return 0;
        }

        let body_width = if state.display_state == DisplayState::Result && has_result_tabs(state) {
            let tab_width = tab_panel_width(width);
            width.saturating_sub(tab_width).max(1)
        } else {
            width
        };

        self.widget.height(body_width)
    }

    fn current_hunk_diff(&self) -> Option<String> {
        let state = self.state.read();
        let edit = state.edit.as_ref()?;
        let total = hunk_total(Some(edit), state.context_radius);
        if total == 0 {
            return None;
        }
        let idx = state.hunk_idx.min(total - 1);
        build_hunk_diff(edit, state.context_radius, idx)
    }

    fn record_auto_accept_diffs(&mut self, edit: &TextEdit) {
        let context_radius = self.state.context_radius;
        let total = hunk_total(Some(edit), context_radius);
        let mut diffs = Vec::with_capacity(total);
        for idx in 0..total {
            if let Some(diff) = build_hunk_diff(edit, context_radius, idx)
                && !diff.is_empty()
            {
                diffs.push(diff);
            }
        }

        let mut state = self.state.write();
        state.applied_diffs = diffs;
        state.rejected_diffs.clear();
        state.pending_apply_diff = None;
        state.edit = None;
        state.result_view = ResultView::Applied;
        state.auto_accept_pending = true;
        drop(state);
        self.rebuild_view();
    }

    fn queue_pending_apply(&mut self) -> bool {
        let diff = self.current_hunk_diff();
        let Some(diff) = diff else {
            return false;
        };
        self.state.write().pending_apply_diff = Some(diff);
        true
    }

    fn record_reject(&mut self) -> bool {
        let diff = self.current_hunk_diff();
        let Some(diff) = diff else {
            return false;
        };
        self.state.write().rejected_diffs.push(diff);
        true
    }

    fn finalize_pending_apply(&mut self, is_error: bool) {
        let mut state = self.state.write();
        let Some(diff) = state.pending_apply_diff.take() else {
            return;
        };
        if is_error {
            state.rejected_diffs.push(diff);
        } else {
            state.applied_diffs.push(diff);
        }
    }

    fn shift_hunk(&mut self, delta: isize) {
        let mut state = self.state.write();
        let total = hunk_total(state.edit.as_ref(), state.context_radius);
        if total == 0 {
            return;
        }
        let mut idx = state.hunk_idx as isize + delta;
        idx = idx.clamp(0, (total - 1) as isize);
        let idx = idx as usize;
        if idx == state.hunk_idx {
            return;
        }
        state.hunk_idx = idx;
        drop(state);
        self.rebuild_view();
    }

    fn adjust_context(&mut self, delta: isize) {
        let mut state = self.state.write();
        let mut radius = state.context_radius as isize + delta;
        radius = radius.clamp(MIN_CONTEXT_RADIUS as isize, MAX_CONTEXT_RADIUS as isize);
        let radius = radius as usize;
        if radius == state.context_radius {
            return;
        }
        state.context_radius = radius;
        clamp_hunk_idx(&mut state);
        drop(state);
        self.rebuild_view();
    }

    fn set_result_view(&mut self, view: ResultView) {
        let mut state = self.state.write();
        if state.result_view == view {
            return;
        }
        state.result_view = view;
        state.collapsed = false;
        drop(state);
        self.rebuild_view();
    }

    fn toggle_collapsed(&mut self) {
        self.state.write().collapsed = !self.state.collapsed;
    }
}

impl Content for StrReplace<'_> {
    fn height(&self, width: u16) -> usize {
        let width = width.max(1);
        let header_height = self.header.line_count(width);
        header_height + self.diff_height(width)
    }

    fn is_actionable(&self) -> bool {
        let state = self.state.read();
        if state.edit.is_some() {
            return true;
        }
        state.display_state == DisplayState::Result && has_result_content(state)
    }

    fn block_with_shortcuts_desc<'b>(&self, block: Block<'b>) -> Block<'b> {
        let state = self.state.read();
        if state.edit.is_some() {
            return block
                .title_top(shortcuts_desc(&[("Apply", "CR")]))
                .title_top(shortcuts_desc(&[("Reject", "Esc")]))
                .title_top(shortcuts_desc(&[("Hunk", "h/l")]))
                .title_top(shortcuts_desc(&[("Context", "[/]")]));
        }

        if state.display_state == DisplayState::Result && has_result_content(state) {
            let toggle_text = if state.collapsed {
                ("Unfold", "z")
            } else {
                ("Fold", "z")
            };
            let block = if has_result_tabs(state) {
                let view = match state.result_view {
                    ResultView::Applied => "Applied",
                    ResultView::Unapplied => "Unapplied",
                };
                block.title_top(shortcuts_desc(&[(view, "1/2")]))
            } else {
                block
            };
            return block.title_top(shortcuts_desc(&[toggle_text]));
        }

        block
    }

    fn reminder_line(&self) -> Option<Line<'static>> {
        let state = self.state.read();
        if state.display_state == DisplayState::Result && state.collapsed {
            Some(Line::from(Span::raw(" (folded)").dark_gray()))
        } else {
            None
        }
    }
}

impl Persistable for StrReplace<'static> {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: Inner = session::load(session)?;
        let mut inst = Self {
            state: State::new(state),
            header: Paragraph::new(""),
            widget: StrReplaceWidget::Empty,
        };
        inst.rebuild_view();
        Ok(inst)
    }
}

impl Component for StrReplace<'static> {
    fn handle_key_event(&mut self, key: &KeyEvent) {
        let (display_state, has_edit, has_tabs, has_content, pending_apply) = {
            let state = self.state.read();
            (
                state.display_state,
                state.edit.is_some(),
                has_result_tabs(state),
                has_result_content(state),
                state.pending_apply_diff.is_some(),
            )
        };

        if display_state == DisplayState::Preview && has_edit {
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Char('h')) => self.shift_hunk(-1),
                (KeyModifiers::NONE, KeyCode::Char('l')) => self.shift_hunk(1),
                (KeyModifiers::NONE, KeyCode::Char('[')) => self.adjust_context(-1),
                (KeyModifiers::NONE, KeyCode::Char(']')) => self.adjust_context(1),
                (KeyModifiers::NONE, KeyCode::Enter) => {
                    if pending_apply {
                        return;
                    }
                    if !self.queue_pending_apply() {
                        return;
                    }
                    let (context_radius, hunk_idx) = {
                        let state = self.state.read();
                        (state.context_radius, effective_hunk_idx(state))
                    };
                    let Some(edit) = self.state.write().edit.take() else {
                        return;
                    };
                    global::action_tx()
                        .send(Action::Tool(ToolAction::ApplyTextEdit {
                            id: self.state.tool_use.id.clone(),
                            name: STR_REPLACE_TOOL_NAME.to_string(),
                            edit,
                            context_radius,
                            hunk_idx,
                            is_rejecting: false,
                        }))
                        .unwrap();
                }
                (KeyModifiers::NONE, KeyCode::Esc) => {
                    if pending_apply {
                        return;
                    }
                    if !self.record_reject() {
                        return;
                    }
                    let (context_radius, hunk_idx) = {
                        let state = self.state.read();
                        (state.context_radius, effective_hunk_idx(state))
                    };
                    let Some(edit) = self.state.write().edit.take() else {
                        return;
                    };
                    global::action_tx()
                        .send(Action::Tool(ToolAction::ApplyTextEdit {
                            id: self.state.tool_use.id.clone(),
                            name: STR_REPLACE_TOOL_NAME.to_string(),
                            edit,
                            context_radius,
                            hunk_idx,
                            is_rejecting: true,
                        }))
                        .unwrap();
                }
                _ => {}
            }
            return;
        }

        if display_state == DisplayState::Result && has_content {
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Char('1')) if has_tabs => {
                    self.set_result_view(ResultView::Applied);
                }
                (KeyModifiers::NONE, KeyCode::Char('2')) if has_tabs => {
                    self.set_result_view(ResultView::Unapplied);
                }
                (KeyModifiers::NONE, KeyCode::Char('z')) => {
                    self.toggle_collapsed();
                }
                _ => {}
            }
        }
    }

    fn handle_event(&mut self, event: &Event) {
        match event {
            Event::Ask(AskEvent::TextEdit {
                edit, auto_accept, ..
            }) => {
                self.finalize_pending_apply(false);
                if *auto_accept {
                    self.record_auto_accept_diffs(edit);
                } else {
                    self.update_text_edit(edit.clone());
                }
            }
            Event::Answer(AnswerEvent::ToolResult {
                is_error, output, ..
            }) => {
                self.finalize_pending_apply(*is_error);
                let message = match output {
                    Final::Message(message) => Some(message.to_owned()),
                    _ => {
                        warn!(?event, "StrReplace tool should only return Final::Message");
                        None
                    }
                };
                let mut state = self.state.write();
                if state.auto_accept_pending {
                    if *is_error && !state.applied_diffs.is_empty() {
                        let applied = std::mem::take(&mut state.applied_diffs);
                        state.rejected_diffs.extend(applied);
                    }
                    state.auto_accept_pending = false;
                }
                state.result_message = message;
                state.result_is_error = Some(*is_error);
                state.display_state = DisplayState::Result;
                state.collapsed = true;
                state.edit = None;
                state.result_view =
                    if state.applied_diffs.is_empty() && !state.rejected_diffs.is_empty() {
                        ResultView::Unapplied
                    } else {
                        ResultView::Applied
                    };
                drop(state);
                self.rebuild_view();
            }
            _ => {
                handle_component_event!(self, event);
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        if area.height == 0 {
            return Ok(());
        }

        use Constraint::Length;

        let width = area.width.max(1);
        let header_height = self.header.line_count(width);
        let diff_height = self.diff_height(width);
        let [area_header, area_body] =
            Layout::vertical([Length(header_height as u16), Length(diff_height as u16)])
                .areas(area);

        frame.render_widget(&self.header, area_header);
        if diff_height == 0 {
            return Ok(());
        }

        let state = self.state.read();
        if state.display_state == DisplayState::Result && has_result_tabs(state) {
            let tab_width = tab_panel_width(width);
            let (area_view, area_tabs) = if tab_width == 0 {
                (area_body, None)
            } else {
                let [view, tabs] =
                    Layout::horizontal([Constraint::Min(1), Constraint::Length(tab_width)])
                        .areas(area_body);
                (view, Some(tabs))
            };

            match &mut self.widget {
                StrReplaceWidget::CodeHighlight(highlight) => {
                    highlight.draw(frame, area_view)?;
                }
                StrReplaceWidget::Paragraph(paragraph) => {
                    frame.render_widget(&*paragraph, area_view);
                }
                StrReplaceWidget::Empty => {}
            }
            if let Some(tabs_area) = area_tabs {
                let tabs_panel = render_tabs_panel(state.result_view);
                frame.render_widget(tabs_panel, tabs_area);
            }
            return Ok(());
        }

        match &mut self.widget {
            StrReplaceWidget::CodeHighlight(highlight) => {
                highlight.draw(frame, area_body)?;
            }
            StrReplaceWidget::Paragraph(paragraph) => {
                frame.render_widget(&*paragraph, area_body);
            }
            StrReplaceWidget::Empty => {}
        }
        Ok(())
    }
}

impl ContentComponent for StrReplace<'static> {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::events::Event;

    fn tool_use() -> ToolUse {
        ToolUse {
            id: "tool_1".to_string(),
            name: STR_REPLACE_TOOL_NAME.to_string(),
            input: json!({
                "path": "src/lib.rs",
                "old_str": "old",
                "new_str": "new",
            }),
        }
    }

    fn text_edit_two_hunks() -> TextEdit {
        let text = [
            "line1", "line2", "line3", "line4", "line5", "line6", "line7", "line8", "line9",
            "line10", "line11", "line12",
        ]
        .join("\n");

        let new_text = [
            "line1",
            "line2-updated",
            "line3",
            "line4",
            "line5",
            "line6",
            "line7",
            "line8",
            "line9",
            "line10",
            "line11-updated",
            "line12",
        ]
        .join("\n");

        TextEdit::new("example.txt".parse().unwrap(), text, new_text)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn str_replace_hunk_navigation_and_context() {
        let tool_use = tool_use();
        let mut widget = StrReplace::new(&tool_use);
        widget.update_text_edit(text_edit_two_hunks());

        assert_eq!(widget.state.hunk_idx, 0);
        widget.handle_key_event(&key(KeyCode::Char('l')));
        assert_eq!(widget.state.hunk_idx, 1);
        widget.handle_key_event(&key(KeyCode::Char('l')));
        assert_eq!(widget.state.hunk_idx, 1);
        widget.handle_key_event(&key(KeyCode::Char('h')));
        assert_eq!(widget.state.hunk_idx, 0);

        let start = widget.state.context_radius;
        widget.handle_key_event(&key(KeyCode::Char(']')));
        assert_eq!(widget.state.context_radius, start + 1);
        widget.handle_key_event(&key(KeyCode::Char('[')));
        assert_eq!(widget.state.context_radius, start);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn str_replace_result_tabs_and_collapse() {
        let tool_use = tool_use();
        let mut widget = StrReplace::new(&tool_use);
        {
            let mut state = widget.state.write();
            state.applied_diffs.push("@@ -1 +1 @@".to_string());
            state.rejected_diffs.push("@@ -2 +2 @@".to_string());
        }

        widget.handle_event(&Event::Answer(AnswerEvent::ToolResult {
            id: "tool_1".to_string(),
            is_error: false,
            is_user_cancelled: false,
            output: Final::Message("Success".to_string()),
        }));

        assert_eq!(widget.state.display_state, DisplayState::Result);
        assert!(widget.state.collapsed);
        assert_eq!(widget.state.result_view, ResultView::Applied);

        widget.handle_key_event(&key(KeyCode::Char('2')));
        assert_eq!(widget.state.result_view, ResultView::Unapplied);
        assert!(!widget.state.collapsed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn str_replace_failed_apply_moves_to_unapplied() {
        let tool_use = tool_use();
        let mut widget = StrReplace::new(&tool_use);
        widget.state.write().pending_apply_diff = Some("@@ -1 +1 @@".to_string());

        widget.handle_event(&Event::Answer(AnswerEvent::ToolResult {
            id: "tool_1".to_string(),
            is_error: true,
            is_user_cancelled: false,
            output: Final::Message("Write failed".to_string()),
        }));

        assert!(widget.state.applied_diffs.is_empty());
        assert_eq!(widget.state.rejected_diffs.len(), 1);
        assert_eq!(widget.state.result_view, ResultView::Unapplied);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn str_replace_apply_success_moves_to_applied_on_next_edit() {
        let tool_use = tool_use();
        let mut widget = StrReplace::new(&tool_use);
        widget.state.write().pending_apply_diff = Some("@@ -1 +1 @@".to_string());

        widget.handle_event(&Event::Ask(AskEvent::TextEdit {
            id: "tool_1".to_string(),
            edit: text_edit_two_hunks(),
            auto_accept: false,
        }));

        assert_eq!(widget.state.applied_diffs.len(), 1);
        assert!(widget.state.pending_apply_diff.is_none());
        assert_eq!(widget.state.display_state, DisplayState::Preview);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn str_replace_auto_accept_collects_applied_diffs() {
        let tool_use = tool_use();
        let mut widget = StrReplace::new(&tool_use);
        let edit = text_edit_two_hunks();
        let total = hunk_total(Some(&edit), widget.state.context_radius);

        widget.handle_event(&Event::Ask(AskEvent::TextEdit {
            id: "tool_1".to_string(),
            edit,
            auto_accept: true,
        }));

        assert_eq!(widget.state.applied_diffs.len(), total);
        assert!(widget.state.rejected_diffs.is_empty());
        assert!(widget.state.pending_apply_diff.is_none());
        assert!(widget.state.edit.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn str_replace_no_tabs_when_only_one_diff_type() {
        let tool_use1 = tool_use();
        let mut widget1 = StrReplace::new(&tool_use1);

        // Test case: only applied diffs exist
        {
            let mut state = widget1.state.write();
            state.applied_diffs.push("@@ -1 +1 @@".to_string());
            state.applied_diffs.push("@@ -3 +3 @@".to_string());
            // rejected_diffs is empty
        }

        widget1.handle_event(&Event::Answer(AnswerEvent::ToolResult {
            id: "tool_1".to_string(),
            is_error: false,
            is_user_cancelled: false,
            output: Final::Message("Success".to_string()),
        }));

        assert_eq!(widget1.state.display_state, DisplayState::Result);
        assert!(widget1.state.collapsed);
        assert_eq!(widget1.state.result_view, ResultView::Applied);

        // Verify no tab view (since only one diff type exists)
        let state = widget1.state.read();
        assert!(!has_result_tabs(state));
        let _ = state;

        // '1'/'2' keys should not respond (no tab view)
        widget1.handle_key_event(&key(KeyCode::Char('1')));
        widget1.handle_key_event(&key(KeyCode::Char('2')));

        // Verify result_view hasn't changed
        assert_eq!(widget1.state.result_view, ResultView::Applied);

        // Test case: only rejected diffs exist
        let tool_use2 = tool_use();
        let mut widget2 = StrReplace::new(&tool_use2);

        {
            let mut state = widget2.state.write();
            // applied_diffs is empty
            state.rejected_diffs.push("@@ -1 +1 @@".to_string());
        }

        widget2.handle_event(&Event::Answer(AnswerEvent::ToolResult {
            id: "tool_2".to_string(),
            is_error: true,
            is_user_cancelled: false,
            output: Final::Message("Failed".to_string()),
        }));

        assert_eq!(widget2.state.display_state, DisplayState::Result);
        assert_eq!(widget2.state.result_view, ResultView::Unapplied);

        // Verify no tab view
        let state = widget2.state.read();
        assert!(!has_result_tabs(state));
        let _ = state;
    }
}
