use bon::bon;
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
    style::Stylize,
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};
use serde_json::Value;
use snafu::prelude::*;
use tracing::warn;

use super::{Component, Content, ContentComponent};
use crate::{
    actions::ToolAction,
    components::{CodeHighlight, shortcuts_desc},
    error::*,
    events::{AnswerEvent, AskEvent, Event},
    global,
};

pub struct Bash<'a> {
    tool_use: ToolUse,
    input: CodeHighlight<'a>,
    requiring_confirmation: bool,
    output: Paragraph<'a>,
}

fn generate_output<'a>(output: Option<BashOutput>) -> Paragraph<'a> {
    let mut lines = vec![];
    let (stderr, stdout) = output
        .map(|output| (output.stderr, output.stdout))
        .unwrap_or_default();

    // '\t' rendering doesn't work well in ratatui.
    // It causes the screen to retain the previous render result in the area of `\t` during scrolling.
    for (prompt, output) in [
        ("2> ".red(), &stderr.replace("\t", "  ")),
        ("1> ".blue(), &stdout.replace("\t", "  ")),
    ] {
        if output.is_empty() {
            continue;
        }
        let mut iter = output.lines().map(String::from); // Convert to owned String
        lines.push(Line::from(vec![prompt, Span::raw(iter.next().unwrap())]));
        for line in iter {
            lines.push(Line::from(line));
        }
    }
    Paragraph::new(lines).wrap(Wrap { trim: false })
}

#[bon]
impl<'a> Bash<'a> {
    #[builder]
    pub fn try_new(tool_use: &ToolUse, output: Option<Value>) -> Result<Self> {
        let input: BashInput = serde_json::from_value(tool_use.input.clone())
            .whatever_context("failed to parse BashInput")?;

        let input = CodeHighlight::try_new(&input.command, Lang::Bash)?;

        let output = output
            .map(serde_json::from_value)
            .transpose()
            .whatever_context("failed to parse BashOutput")?;
        let output = generate_output(output);

        Ok(Self {
            tool_use: tool_use.to_owned(),
            input,
            requiring_confirmation: false,
            output,
        })
    }

    pub fn update_output(&mut self, output: Option<Final>) -> Result<()> {
        if let Some(Final::Json(value)) = output {
            let output =
                serde_json::from_value(value).whatever_context("failed to parse BashOutput")?;
            self.output = generate_output(output);
        }
        Ok(())
    }
}

impl<'a> Content for Bash<'a> {
    fn height(&self, width: u16) -> usize {
        self.input.height(width) + self.output.line_count(width)
    }

    fn is_actionable(&self) -> bool {
        self.requiring_confirmation
    }

    fn block_bottom_with_shortcuts_desc<'b>(&self, block: Block<'b>) -> Block<'b> {
        block
            .title_bottom(shortcuts_desc(&[("Run", "CR")]))
            .title_bottom(shortcuts_desc(&[("Cancel", "Esc")]))
    }
}

impl Component for Bash<'_> {
    fn handle_event(&mut self, event: &Event) {
        match event {
            Event::Ask(AskEvent::ToolUsePermission(_)) => self.requiring_confirmation = true,
            Event::Answer(AnswerEvent::ToolResult { output, .. }) => {
                if let Err(err) = self.update_output(Some(output.to_owned())) {
                    warn!(?err, "failed to update tool output");
                };
            }
            _ => (),
        }
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Enter) => {
                global::action_tx()
                    .send(ToolAction::Grant(self.tool_use.to_owned()).into())
                    .unwrap();
                self.requiring_confirmation = false;
            }
            (KeyModifiers::NONE, KeyCode::Esc) => {
                global::action_tx()
                    .send(ToolAction::Cancel(self.tool_use.to_owned()).into())
                    .unwrap();
                self.requiring_confirmation = false;
            }
            _ => (), // ignore
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::Length;
        let width = area.width;
        let height_input = self.input.height(width);
        let height_output = self.output.line_count(width);

        let [area_input, area_output] =
            Layout::vertical([Length(height_input as u16), Length(height_output as u16)])
                .areas(area);
        self.input.draw(frame, area_input)?;
        frame.render_widget(&self.output, area_output);
        Ok(())
    }
}

impl ContentComponent for Bash<'static> {}
