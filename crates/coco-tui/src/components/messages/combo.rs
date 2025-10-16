use color_eyre::Result;
use ratatui::{
    Frame,
    prelude::Rect,
    style::Color,
    widgets::{Block, Paragraph},
};
use tracing::{debug, warn};

use super::Component;
use crate::{
    actions::{Action, ComboAction},
    events::{ComboEvent, Event},
    global,
};

#[derive(Default)]
pub struct Combo {
    state: State,
}

type State = Option<ComboEvent>;

impl Component for Combo {
    fn handle_event(&mut self, event: &Event) {
        if let Event::Combo(event) = event {
            if matches!(
                self.state,
                Some(ComboEvent::NotFound { .. } | ComboEvent::Executed { .. })
            ) {
                // Already in final state, skip updating
                return;
            }
            self.state = Some(event.to_owned());
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
        let block = Block::bordered().title("Combo");

        match &self.state {
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
                let p = Paragraph::new(format!("Executing starter {name:?}")).style(Color::Yellow);
                frame.render_widget(&p, block.inner(area));
            }
            Some(ComboEvent::Executed { name, starter }) => {
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
        }

        frame.render_widget(&block, area);
        Ok(())
    }
}

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
    let starters = match state {
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
        let starter = code_combo::execute_starter(&starter.path, true).await;
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
