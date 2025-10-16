use std::sync::{Arc, OnceLock};

use tokio::sync::{Mutex, mpsc::UnboundedSender};

use crate::{actions::Action, events::Event};

static EVENT_TX: OnceLock<UnboundedSender<Event>> = OnceLock::new();
static ACTION_TX: OnceLock<UnboundedSender<Action>> = OnceLock::new();

/// Initialize the global event and action senders.
///
/// This function can only be called once during the application's lifetime.
/// Subsequent calls will panic.
///
/// # Arguments
/// * `event_tx` - The unbounded sender for events
/// * `action_tx` - The unbounded sender for actions
pub fn initialize(event_tx: UnboundedSender<Event>, action_tx: UnboundedSender<Action>) {
    EVENT_TX
        .set(event_tx)
        .expect("Event sender has already been initialized");
    ACTION_TX
        .set(action_tx)
        .expect("Action sender has already been initialized");
}

pub fn event_tx() -> UnboundedSender<Event> {
    EVENT_TX
        .get()
        .cloned()
        .expect("Event sender must be initialized")
}

#[allow(dead_code)]
pub fn action_tx() -> UnboundedSender<Action> {
    ACTION_TX
        .get()
        .cloned()
        .expect("Action sender must be initialized")
}

static CONFIG: OnceLock<Arc<Mutex<code_combo::Config>>> = OnceLock::new();

pub async fn config() -> code_combo::Config {
    let config = CONFIG.get_or_init(Default::default);
    config.lock().await.to_owned()
}

pub async fn set_config(config: code_combo::Config) {
    let cell = CONFIG.get_or_init(Default::default);
    let mut cell = cell.lock().await;
    *cell = config;
}
