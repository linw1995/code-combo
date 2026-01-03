use std::sync::{Arc, OnceLock};

use tokio::sync::Mutex;

use crate::Config;

static CONFIG: OnceLock<Arc<Mutex<Option<Config>>>> = OnceLock::new();

pub async fn set_config(config: Config) {
    let cell = CONFIG.get_or_init(|| Arc::new(Mutex::new(None)));
    let mut guard = cell.lock().await;
    *guard = Some(config);
}

pub async fn config() -> Option<Config> {
    let cell = CONFIG.get_or_init(|| Arc::new(Mutex::new(None)));
    let guard = cell.lock().await;
    guard.as_ref().cloned()
}
