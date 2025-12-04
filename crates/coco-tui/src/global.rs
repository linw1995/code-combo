use std::{
    env,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc::UnboundedSender};

use crate::{
    actions::Action,
    events::Event,
    theme::{FinalizedTheme, use_builtin_theme},
};

static EVENT_TX: OnceLock<UnboundedSender<Event>> = OnceLock::new();
static ACTION_TX: OnceLock<UnboundedSender<Action>> = OnceLock::new();
static WORKSPACE_DIR: OnceLock<PathBuf> = OnceLock::new();

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

#[inline]
pub fn event_tx() -> UnboundedSender<Event> {
    EVENT_TX
        .get()
        .cloned()
        .expect("Event sender must be initialized")
}

#[inline]
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

pub fn config_sync() -> code_combo::Config {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async { config().await })
    })
}

pub async fn set_config(config: code_combo::Config) {
    let cell = CONFIG.get_or_init(Default::default);
    let mut cell = cell.lock().await;
    *cell = config;
}

pub fn theme() -> &'static FinalizedTheme {
    let config = config_sync();
    use_builtin_theme(&config.ui.theme)
}

/// Returns the workspace directory by walking up from the current directory
/// until a `.git` directory is found. If no `.git` directory is found,
/// falls back to the current directory.
///
/// The result is cached globally after the first call.
pub fn workspace_dir() -> &'static Path {
    WORKSPACE_DIR.get_or_init(|| {
        // Start from current directory
        let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        // Walk up the directory tree to find a .git directory
        let mut dir = current_dir.clone();
        loop {
            let git_dir = dir.join(".git");
            if git_dir.exists() && git_dir.is_dir() {
                return dir;
            }

            // Move to parent directory
            if let Some(parent) = dir.parent() {
                dir = parent.to_path_buf();
            } else {
                // Reached root, use current directory
                break;
            }
        }

        // Fallback to current directory
        current_dir
    })
}

/// Signal dirty for re-rendering.
///
/// This function sends a `Dirty` event to trigger a re-render of the UI.
/// It's typically called automatically when state is modified through a `WriteGuard`.
#[inline]
pub fn signal_ditry() {
    event_tx().send(Event::Dirty).ok();
}

/// `State` is modify-aware to signal the Dirty event for re-rendering.
///
/// This struct wraps any type `T` and provides a mechanism to automatically
/// trigger a `Dirty` event when the inner value is modified through a write guard.
/// This is useful for tracking state changes that require UI re-rendering.
///
/// The `write()` method returns a `WriteGuard` that implements `DerefMut` for
/// mutable access to the inner value. When the `WriteGuard` is dropped, it
/// automatically sends a `Dirty` event to notify the system that the state
/// has been modified.
#[derive(Debug, Serialize, Deserialize)]
pub struct State<T> {
    #[serde(flatten)]
    inner: T,
}

impl<T> State<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    pub fn write(&mut self) -> WriteGuard<&mut T> {
        WriteGuard {
            inner: &mut self.inner,
        }
    }

    pub fn write_untracked(&mut self) -> &mut T {
        &mut self.inner
    }

    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.inner.to_owned()
    }

    pub fn read(&self) -> &T {
        &self.inner
    }
}

impl<T> Deref for State<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> Default for State<T>
where
    T: Default,
{
    fn default() -> Self {
        Self {
            inner: T::default(),
        }
    }
}

pub struct WriteGuard<T> {
    inner: T,
}

impl<T> Drop for WriteGuard<T> {
    fn drop(&mut self) {
        signal_ditry();
    }
}

impl<T> Deref for WriteGuard<&mut T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner
    }
}

impl<T> DerefMut for WriteGuard<&mut T> {
    fn deref_mut(&mut self) -> &mut T {
        self.inner
    }
}
