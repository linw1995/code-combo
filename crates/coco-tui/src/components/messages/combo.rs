use std::collections::VecDeque;

use color_eyre::Result;
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    prelude::Rect,
    style::Color,
    text::Line,
    widgets::{Block, Borders, Paragraph},
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

impl Combo {
    fn on_indicator_update(&mut self, event: &ComboEvent) {
        match &event {
            ComboEvent::Discovering | ComboEvent::Executing { .. } => (),
            ComboEvent::Discovered { .. }
            | ComboEvent::Executed { .. }
            | ComboEvent::NotFound { .. } => (),
            _ => {} // ignore
        }
    }

    fn on_output_event(&mut self, target: &String, batch: &Vec<code_combo::Line>) {
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
    }
}

impl Content for Combo {
    fn height(&self) -> usize {
        let border_height = 1;
        match self.state.event {
            Some(ComboEvent::Executing { .. } | ComboEvent::Executed { .. }) => {
                self.state.output.len() + border_height
            }
            _ => border_height,
        }
    }
}

impl Component for Combo {
    fn handle_event(&mut self, event: &Event) {
        if let Event::Combo(event) = event {
            if let ComboEvent::Output { name, lines } = event {
                self.on_output_event(name, lines);
            } else {
                if matches!(
                    self.state.event,
                    Some(ComboEvent::NotFound { .. } | ComboEvent::Executed { .. })
                ) {
                    // Already in final state, skip updating
                    return;
                }
                self.on_indicator_update(event);
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

        // Use block title to show progress message and indicator with simple loading character
        let title = match &self.state.event {
            Some(ComboEvent::Discovering) => {
                Line::from("Discovering combo starters...").style(Color::Yellow)
            }
            Some(ComboEvent::Discovered { starters }) => {
                Line::from(format!("Discovered {} combo starters", starters.len()))
                    .style(Color::Green)
            }
            Some(ComboEvent::Executing { name }) => {
                Line::from(format!("Executing combo starter {name:?}...")).style(Color::Yellow)
            }
            Some(ComboEvent::Executed { name, starter }) => Line::from(format!(
                "Executed combo starter {name:?}, success: {}",
                starter.combo.is_ok()
            ))
            .style(Color::Green),
            Some(ComboEvent::NotFound { name }) => {
                Line::from(format!("Combo starter {name:?} is not found")).style(Color::Red)
            }
            None => Line::from("No action is executed"),
            Some(ComboEvent::Output { .. }) => {
                unreachable!()
            }
        };
        let block = Block::new().borders(Borders::TOP).title(title);
        let output_area = block.inner(area);

        if let Some(ComboEvent::Executing { .. } | ComboEvent::Executed { .. }) = &self.state.event
        {
            let chunks =
                Layout::vertical(self.state.output.iter().map(|_| Length(1))).split(output_area);
            for (idx, line) in self.state.output.iter().enumerate() {
                let p = Paragraph::new(line.content.to_string());
                frame.render_widget(&p, chunks[idx]);
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
