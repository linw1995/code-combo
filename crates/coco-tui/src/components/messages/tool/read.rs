use coco_macro::{ComponentExt, ContentComponentExt};
use code_combo::{
    ToolUse,
    tools::{DEFAULT_LINE_LIMIT, DEFAULT_LINE_OFFSET, Final, ReadInput},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    prelude::Rect,
    style::Stylize,
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Wrap},
};
use serde::{Deserialize, Serialize};

use crate::{
    components::{Component, Content, ContentComponent, Persistable},
    error::Result,
    events::{AnswerEvent, Event},
    global::State,
    session::{self, Session},
};

#[derive(Serialize, Deserialize)]
struct Inner {
    input: ReadInput,
    output: Option<String>,
    collapsed: bool,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "read")]
pub struct Read<'a> {
    state: State<Inner>,

    input_widget: Paragraph<'a>,
    output_widget: Option<Paragraph<'a>>,
    output_line_cnt: usize,
}

const COLLAPSED_PREVIEW_LINES: usize = 5;

fn generate_input_widget<'a>(input: ReadInput) -> Paragraph<'a> {
    let mut lines = vec![];

    // Display path
    lines.push(Line::from(vec![
        "  ".yellow(),
        " Path: ".blue(),
        Span::raw(input.path),
    ]));

    // Display line_offset if not default
    if input.line_offset != DEFAULT_LINE_OFFSET {
        lines.push(Line::from(vec![
            " Line Offset: ".blue(),
            Span::raw(input.line_offset.to_string()),
        ]));
    }

    // Display line_limit if not default
    if input.line_limit != DEFAULT_LINE_LIMIT {
        lines.push(Line::from(vec![
            " Line Limit: ".blue(),
            Span::raw(input.line_limit.to_string()),
        ]));
    }

    Paragraph::new(lines).wrap(Wrap { trim: false })
}

fn generate_output_widget<'a>(output: String) -> Paragraph<'a> {
    Paragraph::new(Text::from(output)).wrap(Wrap { trim: false })
}

impl Read<'_> {
    pub fn new(tool_use: &ToolUse) -> Self {
        let input: ReadInput = serde_json::from_value(tool_use.input.to_owned())
            .expect("Should be a valid ReadInput.");
        let input_widget = generate_input_widget(input.clone());

        Self {
            input_widget,
            output_widget: None,
            output_line_cnt: 0,
            state: State::new(Inner {
                input,
                output: None,
                collapsed: true,
            }),
        }
    }

    fn update_output(&mut self, output: Final) {
        let Final::Message(text) = output else {
            unreachable!("Read tool should only return Final::Message");
        };

        self.state.write().output = Some(text.clone());

        self.output_line_cnt = text.lines().count();
        self.output_widget = Some(generate_output_widget(text));
    }

    fn toggle_collapsed(&mut self) {
        self.state.write().collapsed = !self.state.collapsed;
    }
}

impl Content for Read<'_> {
    fn height(&self, width: u16) -> usize {
        let input_height = self.input_widget.line_count(width);
        let output_height = self
            .output_widget
            .as_ref()
            .map(|x| {
                let mut height = x.line_count(width);
                if self.state.collapsed && height > COLLAPSED_PREVIEW_LINES {
                    // Add 1 for the collapsed indicator at the bottom.
                    height = COLLAPSED_PREVIEW_LINES + 1
                };
                height
            })
            .unwrap_or_default();
        input_height + output_height
    }

    fn is_actionable(&self) -> bool {
        self.output_widget.is_some()
    }

    fn block_with_shortcuts_desc<'b>(&self, block: Block<'b>) -> Block<'b> {
        if self.output_widget.is_some() {
            let toggle_text = if self.state.collapsed {
                ("Unfold", "z")
            } else {
                ("Fold", "z")
            };
            block.title_bottom(crate::components::shortcuts_desc(&[toggle_text]))
        } else {
            block
        }
    }
}

impl Persistable for Read<'static> {
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

impl Component for Read<'static> {
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
            && self.output_widget.is_some()
        {
            self.toggle_collapsed();
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::{Length, Min};
        let width = area.width;
        let height_input = self.input_widget.line_count(width);

        if let Some(output_widget) = &self.output_widget {
            let width = area.width;
            let [area_input, area_output] =
                Layout::vertical([Length(height_input as u16), Min(1)]).areas(area);

            // Draw input area
            frame.render_widget(&self.input_widget, area_input);

            // Draw output area
            let height = output_widget.line_count(width);
            let indicator = Paragraph::new(
                format!("... (showing first part of {} lines)", self.output_line_cnt).dark_gray(),
            );
            let indicator_height = indicator.line_count(width);
            if self.state.collapsed && height > COLLAPSED_PREVIEW_LINES + indicator_height {
                let [area_output, area_indicator] =
                    Layout::vertical([Min(1), Length(indicator_height as u16)]).areas(area_output);
                frame.render_widget(indicator, area_indicator);
                frame.render_widget(output_widget, area_output);
            } else {
                frame.render_widget(output_widget, area_output);
            }
        } else {
            frame.render_widget(&self.input_widget, area);
        }

        Ok(())
    }
}

impl ContentComponent for Read<'static> {}
