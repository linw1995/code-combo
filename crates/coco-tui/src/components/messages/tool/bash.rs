use bon::bon;
use code_combo::{BashInput, BashOutput};
use code_highlight::Lang;
use color_eyre::Result;
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    prelude::Rect,
    style::Stylize,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use serde_json::Value;

use crate::{components::CodeHighlight, global};

use super::{Component, Content, ContentComponent};
pub struct Bash<'a> {
    input: CodeHighlight<'a>,
    output: Paragraph<'a>,
}

fn generate_output<'a>(output: Option<BashOutput>) -> Paragraph<'a> {
    let mut lines = vec![];
    let (stderr, stdout) = output
        .map(|output| (output.stderr, output.stdout))
        .unwrap_or_default();

    for (prompt, output) in [("2> ".red(), &stderr), ("1> ".blue(), &stdout)] {
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
    pub fn try_new(input: Value, output: Option<Value>) -> Result<Self> {
        let input: BashInput = serde_json::from_value(input)?;

        let config = global::config_sync();
        let input = CodeHighlight::try_new(&input.command, Lang::Bash, &config.ui.colorschema)?;

        let output = output.map(serde_json::from_value).transpose()?;
        let output = generate_output(output);

        Ok(Self { input, output })
    }

    pub fn update_output(&mut self, output: Option<Value>) -> Result<()> {
        let output = output.map(serde_json::from_value).transpose()?;
        self.output = generate_output(output);
        Ok(())
    }
}

impl<'a> Content for Bash<'a> {
    fn height(&self, width: u16) -> usize {
        self.input.height(width) + self.output.line_count(width)
    }
}

impl Component for Bash<'_> {
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
