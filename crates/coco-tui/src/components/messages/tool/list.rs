use coco_macro::{ComponentExt, ContentComponentExt};
use code_combo::{
    ToolUse,
    tools::{DEFAULT_ENTRY_LIMIT, Final, ListInput},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    prelude::Rect,
    style::Stylize,
    text::{Line, Span, Text},
    widgets::{Block, Wrap},
};
use serde::{Deserialize, Serialize};

use super::super::fold::FoldState;
use crate::{
    actions::Action,
    components::{Component, Content, ContentComponent, Persistable},
    error::Result,
    events::{AnswerEvent, Event},
    global::State,
    session::{self, Session},
    widgets::Paragraph,
};

#[derive(Serialize, Deserialize)]
struct Inner {
    input: ListInput,
    output: Option<String>,
    #[serde(default)]
    display_state: FoldState,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "list")]
pub struct List<'a> {
    state: State<Inner>,

    input_widget: Paragraph<'a>,
    output_widget: Option<Paragraph<'a>>,
    output_line_cnt: usize,
}

const OMITTED_PREVIEW_LINES: usize = 5;

fn generate_input_widget<'a>(input: ListInput) -> Paragraph<'a> {
    let mut lines = vec![];

    lines.push(Line::from(vec![" Path: ".blue(), Span::raw(input.path)]));

    if input.entry_limit != DEFAULT_ENTRY_LIMIT {
        lines.push(Line::from(vec![
            " Entry Limit: ".blue(),
            Span::raw(input.entry_limit.to_string()),
        ]));
    }

    Paragraph::new_wrap(lines, Wrap { trim: false })
}

fn generate_output_widget<'a>(output: String) -> Paragraph<'a> {
    Paragraph::new_wrap(Text::from(output), Wrap { trim: false })
}

impl List<'_> {
    pub fn new(tool_use: &ToolUse) -> Self {
        let input: ListInput = serde_json::from_value(tool_use.input.to_owned())
            .expect("Should be a valid ListInput.");
        let input_widget = generate_input_widget(input.clone());

        Self {
            input_widget,
            output_widget: None,
            output_line_cnt: 0,
            state: State::new(Inner {
                input,
                output: None,
                display_state: FoldState::Preview,
            }),
        }
    }

    fn update_output(&mut self, output: Final) {
        let Final::Message(text) = output else {
            unreachable!("List tool should only return Final::Message");
        };

        self.state.write().output = Some(text.clone());

        self.output_line_cnt = text.lines().count();
        self.output_widget = Some(generate_output_widget(text));
    }

    fn toggle_display_state(&mut self) {
        let mut state = self.state.write();
        state.display_state = state.display_state.toggle();
    }

    fn on_blur(&mut self) {
        if self.state.display_state.is_preview() {
            self.state.write().display_state.collapse();
        }
    }

    fn omitted_indicator(&self) -> Paragraph<'static> {
        Paragraph::new(
            format!(
                "... (omitted, showing first part of {} lines)",
                self.output_line_cnt
            )
            .dark_gray(),
        )
    }
}

impl Content for List<'_> {
    fn height(&self, width: u16) -> usize {
        let input_height = self.input_widget.line_count(width);
        let output_height = self
            .output_widget
            .as_ref()
            .map(|widget| match self.state.display_state {
                FoldState::Collapsed => 0,
                FoldState::Expanded => widget.line_count(width),
                FoldState::Preview => {
                    let height = widget.line_count(width);
                    let indicator = self.omitted_indicator();
                    let indicator_height = indicator.line_count(width);
                    if height > OMITTED_PREVIEW_LINES + indicator_height {
                        OMITTED_PREVIEW_LINES + indicator_height
                    } else {
                        height
                    }
                }
            })
            .unwrap_or_default();
        input_height + output_height
    }

    fn is_actionable(&self) -> bool {
        self.output_widget.is_some()
            && matches!(
                self.state.display_state,
                FoldState::Collapsed | FoldState::Expanded
            )
    }

    fn block_with_shortcuts_desc<'b>(&self, block: Block<'b>) -> Block<'b> {
        if !self.is_actionable() {
            return block;
        }
        let toggle_text = match self.state.display_state {
            FoldState::Collapsed => ("Unfold", "z"),
            FoldState::Expanded => ("Fold", "z"),
            FoldState::Preview => return block,
        };
        block.title_bottom(crate::components::shortcuts_desc(&[toggle_text]))
    }

    fn reminder_line(&self) -> Option<Line<'static>> {
        if self.state.display_state.is_collapsed() {
            Some(Line::from(Span::raw(" (folded)").dark_gray()))
        } else {
            None
        }
    }
}

impl Persistable for List<'static> {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: Inner = session::load(session)?;
        Ok(Self {
            input_widget: generate_input_widget(state.input.clone()),
            output_widget: state.output.clone().map(generate_output_widget),
            output_line_cnt: state
                .output
                .as_ref()
                .map(|x| x.lines().count())
                .unwrap_or_default(),
            state: State::new(state),
        })
    }
}

impl Component for List<'static> {
    fn handle_event(&mut self, event: &Event) {
        match event {
            Event::Answer(AnswerEvent::ToolResult { output, .. }) => {
                self.update_output(output.to_owned());
            }
            _ => {
                handle_component_event!(self, event);
            }
        }
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        if let (KeyModifiers::NONE, KeyCode::Char('z')) = (key.modifiers, key.code)
            && self.is_actionable()
        {
            self.toggle_display_state();
        }
    }

    fn update(&mut self, action: &Action) {
        if matches!(action, Action::Blur) {
            self.on_blur();
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::{Length, Min};
        let width = area.width;
        let height_input = self.input_widget.line_count(width);

        let Some(output_widget) = &self.output_widget else {
            frame.render_widget(&self.input_widget, area);
            return Ok(());
        };

        if self.state.display_state == FoldState::Collapsed {
            frame.render_widget(&self.input_widget, area);
            return Ok(());
        }

        let [area_input, area_output] =
            Layout::vertical([Length(height_input as u16), Min(1)]).areas(area);

        frame.render_widget(&self.input_widget, area_input);

        match self.state.display_state {
            FoldState::Expanded => {
                frame.render_widget(output_widget, area_output);
            }
            FoldState::Preview => {
                let height = output_widget.line_count(width);
                let indicator = self.omitted_indicator();
                let indicator_height = indicator.line_count(width);
                if height > OMITTED_PREVIEW_LINES + indicator_height {
                    let [area_output, area_indicator] =
                        Layout::vertical([Min(1), Length(indicator_height as u16)])
                            .areas(area_output);
                    frame.render_widget(indicator, area_indicator);
                    frame.render_widget(output_widget, area_output);
                } else {
                    frame.render_widget(output_widget, area_output);
                }
            }
            FoldState::Collapsed => (),
        }

        Ok(())
    }
}

impl ContentComponent for List<'static> {}
