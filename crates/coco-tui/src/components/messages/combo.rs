use std::collections::VecDeque;

use color_eyre::Result;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout},
    prelude::Rect,
    style::Stylize,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tracing::{debug, warn};

use super::{Component, Content, ContentComponent, Plain};
use crate::{
    actions::{Action, ComboAction},
    events::{ComboEvent, Event},
    global,
};

#[derive(Default)]
pub struct Combo<'a> {
    state: State,
    widget: Option<Plain<'a>>,
}

#[derive(Default, Clone)]
struct State {
    event: Option<ComboEvent>,
    output: VecDeque<code_combo::Line>,
}

const LIMIT: usize = 10;

impl<'a> Combo<'a> {
    fn update_event_state(&mut self, event: &ComboEvent) {
        // Skip updating if in final state.
        if matches!(
            self.state.event,
            Some(ComboEvent::NotFound { .. } | ComboEvent::Executed { .. })
        ) {
            return;
        }
        let new_state = Some(event.to_owned());
        debug!(?self.state.event, ?new_state, "update event state");
        self.state.event = new_state;
    }

    fn on_combo_event(&mut self, event: &ComboEvent) {
        match event {
            ComboEvent::Output { name, lines } => self.on_output_event(name, lines),
            ComboEvent::Discovering
            | ComboEvent::Executing { .. }
            | ComboEvent::Discovered { .. }
            | ComboEvent::NotFound { .. } => {
                self.update_event_state(event);
            }
            ComboEvent::Executed { starter, .. } => {
                let combo = starter.combo.as_ref().unwrap();
                self.update_plain_msg(combo);
                self.update_event_state(event);
            }
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

    fn get_title_spans(&self) -> Vec<Span<'_>> {
        // Use block title to show progress message and indicator with simple loading character
        let mut spans = vec![" 󱐋 ".yellow(), " Combo: ".into()];
        match &self.state.event {
            Some(ComboEvent::Discovering) => {
                spans.push("  Discovering combo starters...".yellow())
            }
            Some(ComboEvent::Discovered { starters }) => {
                spans.push(format!("  Discovered {} combo starters", starters.len()).green())
            }
            Some(ComboEvent::Executing { name }) => {
                spans.push(name.as_str().cyan());
                spans.push("   Executing...".yellow());
            }
            Some(ComboEvent::Executed { name, starter }) => {
                spans.push(name.as_str().cyan());
                spans.push(if starter.combo.is_ok() {
                    "   Completed".green()
                } else {
                    "   Failed".red()
                });
            }
            Some(ComboEvent::NotFound { name }) => {
                spans.push(name.as_str().cyan());
                spans.push("   Not found".red())
            }
            None => spans.push("  Null".into()),
            Some(ComboEvent::Output { .. }) => {
                unreachable!()
            }
        }
        spans.push(" ".into());
        spans
    }

    fn update_plain_msg(&mut self, combo: &code_combo::Combo) {
        let text = combo.to_markdown();
        self.widget = Some(Plain::new(text))
    }
}

impl<'a> Content for Combo<'a> {
    fn height(&self, width: u16) -> usize {
        let border_height = 1;
        if let Some(plain) = &self.widget {
            plain.height(width) + border_height
        } else {
            match self.state.event {
                Some(ComboEvent::Executing { .. } | ComboEvent::Executed { .. }) => {
                    self.state.output.len() + border_height
                }
                _ => border_height,
            }
        }
    }
}

impl<'a> Component for Combo<'a> {
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
                    tokio::task::spawn(execute(name.to_owned(), self.state.clone()));
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
        } else if let Some(ComboEvent::Executing { .. } | ComboEvent::Executed { .. }) =
            &self.state.event
        {
            let chunks =
                Layout::vertical(self.state.output.iter().map(|_| Length(1))).split(output_area);
            for (idx, line) in self.state.output.iter().enumerate() {
                let p = Paragraph::new(line.content.to_string());
                frame.render_widget(&p, chunks[idx]);
            }
        }

        Ok(())
    }
}

impl ContentComponent for Combo<'static> {}

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
