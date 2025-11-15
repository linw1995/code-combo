use bon::bon;
use code_combo::{BashInput, BashOutput};
use color_eyre::Result;
use ratatui::{
    Frame,
    prelude::Rect,
    style::Stylize,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use serde_json::Value;

use super::{Component, Content, ContentComponent};

pub struct Bash<'a> {
    input: BashInput,
    command: Paragraph<'a>,
}

fn generate_command<'a>(input: &BashInput, output: Option<BashOutput>) -> Paragraph<'a> {
    let mut lines = vec![];
    let (stderr, stdout) = output
        .map(|output| (output.stderr, output.stdout))
        .unwrap_or_default();

    for (prompt, output) in [
        ("$ ".green(), &input.command),
        ("2> ".red(), &stderr),
        ("1> ".blue(), &stdout),
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
    pub fn try_new(input: Value, output: Option<Value>) -> Result<Self> {
        let input = serde_json::from_value(input)?;
        let output = output.map(serde_json::from_value).transpose()?;
        let command = generate_command(&input, output);
        Ok(Self { input, command })
    }

    pub fn update_output(&mut self, output: Option<Value>) -> Result<()> {
        let output = output.map(serde_json::from_value).transpose()?;
        self.command = generate_command(&self.input, output);
        Ok(())
    }
}

impl<'a> Content for Bash<'a> {
    fn height(&self, width: u16) -> usize {
        self.command.line_count(width)
    }
}

impl Component for Bash<'_> {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        frame.render_widget(self.command.clone(), area);
        Ok(())
    }
}

impl ContentComponent for Bash<'static> {}
