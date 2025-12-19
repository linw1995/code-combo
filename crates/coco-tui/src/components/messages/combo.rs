use std::collections::VecDeque;

use coco_macro::{ComponentExt, ContentComponentExt};
use code_combo::{SessionEnv, StarterCommand};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout},
    prelude::Rect,
    style::Stylize,
    text::{Line, Span},
    widgets::{Block, Borders},
};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::{
    actions::{Action, ComboAction},
    components::{Component, Content, ContentComponent, Persistable, Plain},
    error::*,
    events::{ComboEvent, Event},
    global::{self, State},
    session::{self, Session},
    widgets::Paragraph,
};

#[derive(Serialize, Deserialize)]
enum StarterState {
    Discovering,
    NotFound,
    Executing { output: VecDeque<DisplayedLine> },
    Finalized { output: String },
}

#[derive(Clone, Serialize, Deserialize)]
struct DisplayedLine {
    stream: code_combo::StreamKind,
    text: String,
}

#[derive(Serialize, Deserialize)]
struct Inner {
    name: String,
    is_error: bool,
    starter_state: StarterState,
    #[serde(default = "default_collapsed")]
    collapsed: bool,
}

impl Default for StarterState {
    fn default() -> Self {
        Self::Discovering
    }
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            name: String::new(),
            is_error: false,
            starter_state: StarterState::default(),
            collapsed: default_collapsed(),
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

fn default_collapsed() -> bool {
    true
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
                        output: VecDeque::new(),
                    };
                    state.collapsed = false;
                }
            }
            ComboEvent::Executed { name, starter, .. } => {
                if &self.state.name == name {
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
                    state.collapsed = true;
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
        for text in &chunk.lines {
            while lines.len() >= LIMIT {
                debug!(
                    limit = LIMIT,
                    "Line count exceeds limit, removing oldest lines"
                );
                lines.pop_front();
            }
            lines.push_back(DisplayedLine {
                stream: chunk.stream,
                text: text.clone(),
            });
        }
    }

    fn toggle_collapsed(&mut self) {
        self.state.write().collapsed = !self.state.collapsed;
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
        if self.has_collapsible_body() && self.state.collapsed {
            spans.push(" (folded)".dark_gray());
        }
        spans.push(" ".into());
        spans
    }
}

impl Content for Combo {
    fn height(&self, width: u16) -> usize {
        let border_height = 1;
        if self.has_collapsible_body() && self.state.collapsed {
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
        let toggle_text = if self.state.collapsed {
            ("Unfold", "e")
        } else {
            ("Fold", "e")
        };
        block.title_bottom(crate::components::shortcuts_desc(&[toggle_text]))
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

        if let (KeyModifiers::NONE, KeyCode::Char('e')) = (key.modifiers, key.code) {
            self.toggle_collapsed();
            return;
        }

        if !self.state.collapsed
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
        debug!(?action, "updating");
        if let Action::Combo(combo) = action {
            match combo {
                ComboAction::Discover => {
                    tokio::task::spawn(discover());
                }
                ComboAction::Execute { name } => {
                    tokio::task::spawn(execute(name.to_owned()));
                }
            }
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

        if self.has_collapsible_body() && self.state.collapsed {
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

async fn discover() {
    let tx = global::event_tx();
    let config = global::config().await;
    let combo_dir = config.combo_dir();

    tx.send(ComboEvent::Discovering.into()).unwrap();
    let starters = code_combo::discover_combo_starters(&combo_dir.to_string_lossy()).await;
    tx.send(ComboEvent::Discovered { starters }.into()).unwrap();
}

async fn execute(name: String) {
    let tx = global::event_tx();
    let config = global::config().await;
    let combo_dir = config.combo_dir();
    let starters = {
        tx.send(ComboEvent::Discovering.into()).unwrap();
        let starters = code_combo::discover_combo_starters(&combo_dir.to_string_lossy()).await;
        tx.send(
            ComboEvent::Discovered {
                starters: starters.clone(),
            }
            .into(),
        )
        .unwrap();
        starters
    };
    if let Some(starter) = starters.into_iter().find(|starter| match &starter.combo {
        Ok(combo) => combo.metadata.name == name,
        Err(err) => {
            warn!(?starter.path, ?err,"Failed to load combo");
            false
        }
    }) && let Ok(_combo) = &starter.combo
    {
        tx.send(ComboEvent::Executing { name: name.clone() }.into())
            .unwrap();

        let session_env = SessionEnv::builder()
            .build()
            .expect("failed to build session");
        let mut execution = StarterCommand::new(&starter.path)
            .session_env(session_env)
            .execute();
        while let Some(event) = execution.next().await {
            match event {
                code_combo::StarterEvent::Output { chunk } => {
                    tx.send(
                        ComboEvent::Output {
                            name: name.clone(),
                            chunk,
                        }
                        .into(),
                    )
                    .unwrap();
                }
                code_combo::StarterEvent::Failed { reason } => {
                    tx.send(
                        ComboEvent::Executed {
                            name: name.clone(),
                            starter: code_combo::Starter {
                                path: starter.path.clone(),
                                combo: Err(code_combo::StarterError::Invalid { reason }),
                            },
                        }
                        .into(),
                    )
                    .unwrap();
                    return;
                }
                _ => (),
            }
        }
        let starter = execution.wait().await.unwrap();
        tx.send(
            ComboEvent::Executed {
                name: name.clone(),
                starter,
            }
            .into(),
        )
        .unwrap();
    } else {
        tx.send(ComboEvent::NotFound { name: name.clone() }.into())
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use crate::events::{ComboEvent, Event};

    use super::*;

    fn test_key_e() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE)
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
    async fn combo_is_collapsed_by_default_and_toggles_with_e() {
        let mut combo = Combo::new("demo");
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            name: "demo".to_string(),
            starter: make_starter("demo"),
        }));

        assert_eq!(combo.height(80), 1);
        combo.handle_key_event(&test_key_e());
        assert!(combo.height(80) > 1);
        combo.handle_key_event(&test_key_e());
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
        combo.handle_key_event(&test_key_e());
        assert_eq!(combo.height(80), 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn combo_persists_collapsed_state() {
        let mut combo = Combo::new("demo");
        combo.handle_event(&Event::Combo(ComboEvent::Executed {
            name: "demo".to_string(),
            starter: make_starter("demo"),
        }));
        combo.handle_key_event(&test_key_e());
        assert!(combo.height(80) > 1);

        let session = combo.save();
        let loaded = Combo::load(session).unwrap();
        assert!(loaded.height(80) > 1);
    }
}
