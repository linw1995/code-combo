use coco_macro::{ComponentExt, ContentComponentExt};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout},
    prelude::Rect,
    style::Stylize,
    text::{Line, Span},
    widgets::{Block, Borders},
};
use serde::{Deserialize, Serialize};

use super::fold::FoldState;
use super::streaming::StreamedLines;
use crate::{
    actions::Action,
    components::{Component, Content, ContentComponent, Persistable, Plain},
    error::*,
    events::{ComboEvent, Event},
    global::State,
    session::{self, Session},
    widgets::Paragraph,
};

#[derive(Serialize, Deserialize, Default)]
enum StarterState {
    #[default]
    Discovering,
    NotFound,
    Cancelled,
    Executing {
        output: StreamedLines,
    },
    Finalized {
        output: String,
    },
}

#[derive(Serialize, Deserialize)]
struct Inner {
    name: String,
    is_error: bool,
    starter_state: StarterState,
    #[serde(default = "default_display_state")]
    display_state: FoldState,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            name: String::new(),
            is_error: false,
            starter_state: StarterState::default(),
            display_state: default_display_state(),
        }
    }
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "combo")]
pub struct Combo {
    state: State<Inner>,

    widget: Option<Plain>,
}

const LIMIT: usize = 10;

fn default_display_state() -> FoldState {
    FoldState::Collapsed
}

impl Combo {
    pub fn new(name: &str) -> Self {
        Self {
            state: State::new(Inner {
                name: name.to_string(),
                ..Default::default()
            }),
            widget: None,
        }
    }

    fn has_collapsible_body(&self) -> bool {
        self.widget.is_some()
    }

    fn on_combo_event(&mut self, event: &ComboEvent) {
        match event {
            ComboEvent::NotFound { name } => {
                if &self.state.name == name {
                    self.state.write().starter_state = StarterState::NotFound
                }
            }
            ComboEvent::Output { name, chunk } => self.on_ouput_event(name, chunk),
            ComboEvent::Executing { name } => {
                if &self.state.name == name {
                    let mut state = self.state.write();
                    state.starter_state = StarterState::Executing {
                        output: StreamedLines::new(Some(LIMIT)),
                    };
                    state.display_state.expand();
                }
            }
            ComboEvent::Executed { name, starter, .. } => {
                if &self.state.name == name {
                    {
                        let mut state = self.state.write();
                        let output = match &starter.combo {
                            Ok(combo) => combo.to_markdown(),
                            Err(err) => {
                                state.is_error = true;
                                format!("Failed to execute starter: {err}")
                            }
                        };
                        self.widget = Some(Plain::new(output.clone()));
                        state.starter_state = StarterState::Finalized { output };
                        state.display_state.expand();
                    }
                }
            }
            ComboEvent::Cancelled { name } => {
                if name
                    .as_ref()
                    .map(|name| name == &self.state.name)
                    .unwrap_or(true)
                {
                    {
                        let mut state = self.state.write();
                        state.starter_state = StarterState::Cancelled;
                        state.display_state.expand();
                    }
                }
            }
            _ => (),
        }
    }

    fn on_ouput_event(&mut self, name: &str, chunk: &code_combo::OutputChunk) {
        if self.state.name != name {
            return;
        }
        let StarterState::Executing { output: lines } = &mut self.state.write().starter_state
        else {
            return;
        };
        lines.push_chunk(chunk);
    }

    fn toggle_display_state(&mut self) {
        let mut state = self.state.write();
        state.display_state = state.display_state.toggle();
    }

    fn on_blur(&mut self) {
        if self.has_collapsible_body() {
            self.state.write().display_state.collapse();
        }
    }

    fn get_title_spans(&self) -> Vec<Span<'_>> {
        // Use block title to show progress message and indicator with simple loading character
        let mut spans = vec![" 󱐋 ".yellow(), " Combo:".into()];
        match self.state.starter_state {
            StarterState::Discovering => spans.push("   Discovering combo starters...".yellow()),
            StarterState::NotFound => {
                spans.push(self.state.name.clone().cyan());
                spans.push("   Not found".red())
            }
            StarterState::Cancelled => {
                spans.push(self.state.name.clone().cyan());
                spans.push("   Cancelled".red());
            }
            StarterState::Executing { .. } => {
                spans.push(self.state.name.clone().cyan());
                spans.push("   Executing...".yellow());
            }
            StarterState::Finalized { .. } => {
                spans.push(self.state.name.clone().cyan());
                spans.push(if self.state.is_error {
                    "   Failed".red()
                } else {
                    "   Completed".green()
                });
            }
        }
        if let Some(line) = self.reminder_line() {
            spans.extend(line.spans);
        }
        spans.push(" ".into());
        spans
    }
}

impl Content for Combo {
    fn height(&self, width: u16) -> usize {
        let border_height = 1;
        if self.has_collapsible_body() && self.state.display_state.is_collapsed() {
            return border_height;
        }
        if let Some(plain) = &self.widget {
            plain.height(width) + border_height
        } else {
            (match &self.state.starter_state {
                StarterState::Executing { output } => output.len(),
                _ => 0,
            }) + border_height
        }
    }

    fn is_actionable(&self) -> bool {
        self.has_collapsible_body()
    }

    fn block_with_shortcuts_desc<'a>(&self, block: Block<'a>) -> Block<'a> {
        if !self.has_collapsible_body() {
            return block;
        }
        let toggle_text = if self.state.display_state.is_collapsed() {
            ("Unfold", "z")
        } else {
            ("Fold", "z")
        };
        block.title_bottom(crate::components::shortcuts_desc(&[toggle_text]))
    }

    fn reminder_line(&self) -> Option<Line<'static>> {
        if self.has_collapsible_body() && self.state.display_state.is_collapsed() {
            Some(Line::from(Span::raw(" (folded)").dark_gray()))
        } else {
            None
        }
    }
}

impl Persistable for Combo {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: Inner = session::load(session)?;
        let widget = if let StarterState::Finalized { output } = &state.starter_state {
            Some(Plain::new(output.clone()))
        } else {
            None
        };
        Ok(Self {
            state: State::new(state),
            widget,
        })
    }
}

impl Component for Combo {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(
            self.widget
                .as_mut()
                .map(|m| m as &mut dyn Component)
                .into_iter(),
        )
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        if !self.has_collapsible_body() {
            return;
        }

        if let (KeyModifiers::NONE, KeyCode::Char('z')) = (key.modifiers, key.code) {
            self.toggle_display_state();
            return;
        }

        if !self.state.display_state.is_collapsed()
            && let Some(widget) = &mut self.widget
        {
            widget.handle_key_event(key);
        }
    }

    fn handle_event(&mut self, event: &Event) {
        if let Event::Combo(event) = event {
            self.on_combo_event(event);
        } else {
            handle_component_event!(self, event);
        }
    }

    fn update(&mut self, action: &Action) {
        if let Action::Blur = action {
            self.on_blur()
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::Length;

        let title_spans = self.get_title_spans();
        let block = Block::new()
            .borders(Borders::TOP)
            .title(Line::from("")) // placeholder for border on the left of the actual title
            .title(Line::from(title_spans))
            .title_alignment(Alignment::Left);
        frame.render_widget(&block, area);

        if self.has_collapsible_body() && self.state.display_state.is_collapsed() {
            return Ok(());
        }

        let output_area = block.inner(area);

        if let Some(plain) = &mut self.widget {
            plain.draw(frame, output_area)?;
        } else if let StarterState::Executing { output } = &self.state.starter_state {
            let chunks = Layout::vertical(output.iter().map(|_| Length(1))).split(output_area);
            output.iter().zip(chunks.iter()).for_each(|(line, chunk)| {
                let p = Paragraph::new(line.text.clone());
                frame.render_widget(&p, *chunk);
            });
        }

        Ok(())
    }
}

impl ContentComponent for Combo {}

#[cfg(test)]
mod tests {
    use crate::actions::Action;
    use crate::events::{ComboEvent, Event};

    use super::*;

    fn test_key_z() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE)
    }

    fn make_starter(name: &str) -> code_combo::Starter {
        code_combo::Starter {
            path: "/tmp/demo".to_string(),
            combo: Ok(code_combo::Combo {
                metadata: code_combo::ComboMetadata {
                    name: name.to_string(),
                    description: String::new(),
                    mode: code_combo::ComboMode::Unknown,
                },
                instructions: vec![code_combo::Instruction::Text(
                    "line1\nline2\nline3".to_string(),
                )],
            }),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_is_collapsed_by_default_and_toggles_with_z() {
        let mut combo = Combo::new("demo");
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            name: "demo".to_string(),
            starter: make_starter("demo"),
        }));

        assert!(combo.height(80) > 1);
        combo.handle_action(&Action::Blur);
        assert_eq!(combo.height(80), 1);
        combo.handle_key_event(&test_key_z());
        assert!(combo.height(80) > 1);
        combo.handle_key_event(&test_key_z());
        assert_eq!(combo.height(80), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_is_visible_while_executing() {
        let mut combo = Combo::new("demo");
        combo.handle_event(&Event::Combo(ComboEvent::Executing {
            name: "demo".to_string(),
        }));
        combo.handle_event(&Event::Combo(ComboEvent::Output {
            name: "demo".to_string(),
            chunk: code_combo::OutputChunk {
                timestamp: 0,
                stream: code_combo::StreamKind::Stdout,
                lines: vec!["line1".to_string(), "line2".to_string()],
            },
        }));

        assert_eq!(combo.height(80), 3);
        combo.handle_key_event(&test_key_z());
        assert_eq!(combo.height(80), 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_persists_collapsed_state() {
        let mut combo = Combo::new("demo");
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            name: "demo".to_string(),
            starter: make_starter("demo"),
        }));
        combo.handle_action(&Action::Blur);
        combo.handle_key_event(&test_key_z());
        assert!(combo.height(80) > 1);

        let session = combo.save();
        let loaded = Combo::load(session).unwrap();
        assert!(loaded.height(80) > 1);
    }
}
