use std::collections::VecDeque;

use coco_macro::{ComponentExt, ContentComponentExt};
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
    Executing { output: VecDeque<code_combo::Line> },
    Finalized { output: String },
}

#[derive(Default, Serialize, Deserialize)]
struct Inner {
    name: String,
    is_error: bool,
    starter_state: StarterState,
}

impl Default for StarterState {
    fn default() -> Self {
        Self::Discovering
    }
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "combo")]
pub struct Combo {
    state: State<Inner>,

    widget: Option<Plain>,
}

const LIMIT: usize = 10;

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

    fn on_combo_event(&mut self, event: &ComboEvent) {
        match event {
            ComboEvent::NotFound { name } => {
                if &self.state.name == name {
                    self.state.write().starter_state = StarterState::NotFound
                }
            }
            ComboEvent::Output { name, lines } => self.on_ouput_event(name, lines),
            ComboEvent::Executing { name } => {
                if &self.state.name == name {
                    self.state.write().starter_state = StarterState::Executing {
                        output: VecDeque::new(),
                    };
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
                }
            }
            _ => (),
        }
    }

    fn on_ouput_event(&mut self, name: &str, batch: &Vec<code_combo::Line>) {
        if self.state.name != name {
            return;
        }
        let StarterState::Executing { output: lines } = &mut self.state.write().starter_state
        else {
            return;
        };
        let batch_size = batch.len();
        let line_count = lines.len();
        if batch_size > LIMIT {
            debug!(?LIMIT, ?batch_size, "Batch size exceeds limit, truncating");
            lines.clear();
            lines.extend(batch[batch_size - LIMIT..batch_size].to_vec());
        } else if line_count + batch_size > LIMIT {
            debug!(
                ?line_count,
                ?batch_size,
                ?LIMIT,
                "Line count exceeds limit, removing oldest lines"
            );
            lines.drain(0..(line_count + batch_size - LIMIT));
            lines.extend(batch.to_owned());
        } else {
            debug!(?line_count, ?batch_size, ?LIMIT, "Adding new lines");
            lines.extend(batch.to_owned());
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
        spans.push(" ".into());
        spans
    }
}

impl Content for Combo {
    fn height(&self, width: u16) -> usize {
        let border_height = 1;
        if let Some(plain) = &self.widget {
            plain.height(width) + border_height
        } else {
            (match &self.state.starter_state {
                StarterState::Executing { output } => output.len(),
                _ => 0,
            }) + border_height
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
        let output_area = block.inner(area);

        if let Some(plain) = &mut self.widget {
            plain.draw(frame, output_area)?;
        } else if let StarterState::Executing { output } = &self.state.starter_state {
            let chunks = Layout::vertical(output.iter().map(|_| Length(1))).split(output_area);
            for (idx, line) in output.iter().enumerate() {
                let p = Paragraph::new(line.content.to_string());
                frame.render_widget(&p, chunks[idx]);
            }
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
        let (starter, mut rx) = code_combo::execute_starter(&starter.path, true);
        while let Ok(batch) = rx.recv().await {
            tx.send(
                ComboEvent::Output {
                    name: name.clone(),
                    lines: batch,
                }
                .into(),
            )
            .unwrap();
        }
        tx.send(
            ComboEvent::Executed {
                name: name.clone(),
                starter: starter.await.unwrap(),
            }
            .into(),
        )
        .unwrap();
    } else {
        tx.send(ComboEvent::NotFound { name: name.clone() }.into())
            .unwrap();
    }
}
