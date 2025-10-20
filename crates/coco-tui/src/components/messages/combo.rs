use std::collections::VecDeque;

use color_eyre::Result;
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    prelude::Rect,
    style::Color,
    widgets::{Block, Paragraph},
};
use tracing::{debug, warn};

use super::{Component, Content, ContentComponent};
use crate::{
    actions::{Action, ComboAction},
    events::{ComboEvent, Event},
    global,
};

#[derive(Default)]
pub struct Combo {
    state: State,
}

#[derive(Default, Clone)]
struct State {
    event: Option<ComboEvent>,
    output: VecDeque<code_combo::Line>,
}

const LIMIT: usize = 10;

impl Content for Combo {
    fn height(&self) -> usize {
        LIMIT
    }
}

impl Component for Combo {
    fn handle_event(&mut self, event: &Event) {
        if let Event::Combo(event) = event {
            if let ComboEvent::Output {
                name: target,
                lines: batch,
            } = event
            {
                match &self.state.event {
                    Some(ComboEvent::Executing { name } | ComboEvent::Executed { name, .. })
                        if name == target =>
                    {
                        let lines = &mut self.state.output;
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
                    _ => {
                        // ignore
                    }
                }
            } else {
                if matches!(
                    self.state.event,
                    Some(ComboEvent::NotFound { .. } | ComboEvent::Executed { .. })
                ) {
                    // Already in final state, skip updating
                    return;
                }
                self.state.event = Some(event.to_owned());
            }
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
                    tokio::task::spawn(execute(name.to_owned(), self.state.clone()));
                }
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::Length;

        let block = Block::bordered().title("Combo");
        match &self.state.event {
            Some(ComboEvent::Discovering) => {
                let p = Paragraph::new("Discovering...").style(Color::Yellow);
                frame.render_widget(&p, block.inner(area));
            }
            Some(ComboEvent::Discovered { starters }) => {
                let p = Paragraph::new(format!("Discovered {} starters", starters.len()))
                    .style(Color::Green);
                frame.render_widget(&p, block.inner(area));
            }
            Some(ComboEvent::Executing { name }) => {
                let chunks = Layout::vertical(self.state.output.iter().map(|_| Length(1)))
                    .split(block.inner(area));
                for (idx, line) in self.state.output.iter().enumerate() {
                    let p = Paragraph::new(line.content.to_string());
                    frame.render_widget(&p, chunks[idx]);
                }
                let p = Paragraph::new(format!("Executing starter {name:?}")).style(Color::Yellow);
                frame.render_widget(&p, block.inner(area));
            }
            Some(ComboEvent::Executed { name, starter }) => {
                let chunks = Layout::vertical(self.state.output.iter().map(|_| Length(1)))
                    .split(block.inner(area));
                for (idx, line) in self.state.output.iter().enumerate() {
                    let p = Paragraph::new(line.content.to_string());
                    frame.render_widget(&p, chunks[idx]);
                }
                let p = Paragraph::new(format!(
                    "Executed starter {name:?}, success: {}",
                    starter.combo.is_ok()
                ))
                .style(Color::Green);
                frame.render_widget(&p, block.inner(area));
            }
            Some(ComboEvent::NotFound { name }) => {
                let p = Paragraph::new(format!("Starter {name:?} is not found")).style(Color::Red);
                frame.render_widget(&p, block.inner(area));
            }
            None => {
                let p = Paragraph::new("No action is executed");
                frame.render_widget(&p, block.inner(area));
            }
            Some(ComboEvent::Output { .. }) => {
                unreachable!()
            }
        }

        frame.render_widget(&block, area);
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

async fn execute(name: String, state: State) {
    let tx = global::event_tx();
    let config = global::config().await;
    let combo_dir = config.combo_dir();
    let starters = match state.event {
        Some(ComboEvent::Discovered { starters }) => starters,
        _ => {
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
        }
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
