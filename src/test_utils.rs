use std::{
    path::PathBuf,
    sync::{Mutex, MutexGuard, OnceLock},
};

pub(crate) fn preferred_temp_dir() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("COCO_TEST_TMPDIR") {
        let path = std::path::PathBuf::from(path);
        if path.is_dir() {
            return path;
        }
    }
    let system = std::env::temp_dir();
    if cfg!(unix) {
        let short = std::path::PathBuf::from("/tmp");
        if short.is_dir() && short.to_string_lossy().len() < system.to_string_lossy().len() {
            return short;
        }
    }
    system
}

pub(crate) struct SessionSocketTestGuard {
    _lock: MutexGuard<'static, ()>,
}

impl SessionSocketTestGuard {
    pub(crate) fn acquire() -> Self {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let lock = LOCK.get_or_init(|| Mutex::new(()));
        let guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        Self { _lock: guard }
    }

    pub(crate) fn set_global(&self, path: impl Into<PathBuf>) {
        crate::global::set_session_socket_path(path);
    }

    pub(crate) fn clear_global(&self) {
        crate::global::clear_session_socket_path();
    }

    pub(crate) fn set_env(&self, value: &str) {
        // Safety: test-only process env mutation; serialized by SessionSocketTestGuard lock.
        unsafe {
            std::env::set_var(crate::SESSION_SOCKET_ENV, value);
        }
    }

    pub(crate) fn clear_env(&self) {
        // Safety: test-only process env mutation; serialized by SessionSocketTestGuard lock.
        unsafe {
            std::env::remove_var(crate::SESSION_SOCKET_ENV);
        }
    }
}

impl Drop for SessionSocketTestGuard {
    fn drop(&mut self) {
        crate::global::clear_session_socket_path();
        // Safety: test-only process env mutation; serialized by SessionSocketTestGuard lock.
        unsafe {
            std::env::remove_var(crate::SESSION_SOCKET_ENV);
        }
    }
}
