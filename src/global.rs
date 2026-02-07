use std::{
    path::PathBuf,
    sync::{Arc, OnceLock, RwLock},
};

use tokio::sync::Mutex;

use crate::Config;

static CONFIG: OnceLock<Arc<Mutex<Option<Config>>>> = OnceLock::new();
static SESSION_SOCKET_PATH: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

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

pub fn set_session_socket_path(path: impl Into<PathBuf>) {
    let lock = SESSION_SOCKET_PATH.get_or_init(|| RwLock::new(None));
    let mut guard = lock
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(path.into());
}

pub fn clear_session_socket_path() {
    let lock = SESSION_SOCKET_PATH.get_or_init(|| RwLock::new(None));
    let mut guard = lock
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = None;
}

pub fn session_socket_path() -> Option<PathBuf> {
    let lock = SESSION_SOCKET_PATH.get_or_init(|| RwLock::new(None));
    let guard = lock.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.clone()
}
