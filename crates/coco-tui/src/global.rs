use std::{
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    actions::Action,
    combo_run_bridge::ComboRunBridge,
    events::Event,
    theme::{FinalizedTheme, use_builtin_theme},
};

static EVENT_TX: OnceLock<UnboundedSender<Event>> = OnceLock::new();
static ACTION_TX: OnceLock<UnboundedSender<Action>> = OnceLock::new();
static WORKSPACE_DIR: OnceLock<PathBuf> = OnceLock::new();
static IGNORE_WORKSPACE_SCRIPTS: AtomicBool = AtomicBool::new(false);
static COMBO_RUN_BRIDGE: OnceLock<ComboRunBridge> = OnceLock::new();
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

pub async fn config() -> code_combo::Config {
    code_combo::global::config().await.unwrap_or_default()
}

pub fn config_sync() -> code_combo::Config {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async { config().await })
    })
}

pub async fn set_config(config: code_combo::Config) {
    code_combo::global::set_config(config).await;
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
    WORKSPACE_DIR.get_or_init(code_combo::workspace_dir)
}

pub fn workspace_combo_dir() -> PathBuf {
    workspace_dir().join(".coco/combos")
}

pub fn set_ignore_workspace_scripts(ignore: bool) {
    IGNORE_WORKSPACE_SCRIPTS.store(ignore, Ordering::Relaxed);
}

pub fn ignore_workspace_scripts() -> bool {
    IGNORE_WORKSPACE_SCRIPTS.load(Ordering::Relaxed)
}

pub fn init_combo_run_bridge() -> &'static ComboRunBridge {
    COMBO_RUN_BRIDGE.get_or_init(ComboRunBridge::default)
}

pub fn combo_run_bridge() -> Option<&'static ComboRunBridge> {
    COMBO_RUN_BRIDGE.get()
}

#[inline]
pub fn trigger_schedule_session_save() {
    action_tx().send(Action::schedule_session_save()).ok();
}

/// Signal dirty for re-rendering.
///
/// This function sends a `Dirty` event to trigger a re-render of the UI.
/// It's typically called automatically when state is modified through a `WriteGuard`.
#[inline]
pub fn signal_dirty() {
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

impl<T: Clone> Clone for State<T> {
    fn clone(&self) -> Self {
        Self::new(T::clone(self))
    }
}

pub struct WriteGuard<T> {
    inner: T,
}

impl<T> Drop for WriteGuard<T> {
    fn drop(&mut self) {
        signal_dirty();
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
