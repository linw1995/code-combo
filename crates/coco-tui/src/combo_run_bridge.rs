use std::collections::HashMap;
use std::sync::Mutex;

use code_combo::{ComboRunEvent, ComboRunMessage, ComboRunResult};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Default)]
pub struct ComboRunBridge {
    inner: Mutex<HashMap<String, UnboundedSender<ComboRunMessage>>>,
}

impl ComboRunBridge {
    pub fn register(&self, run_id: String, sender: UnboundedSender<ComboRunMessage>) -> bool {
        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };
        if guard.contains_key(&run_id) {
            return false;
        }
        guard.insert(run_id, sender);
        true
    }

    pub fn send_event(&self, run_id: &str, event: ComboRunEvent) -> bool {
        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };
        let Some(sender) = guard.get(run_id) else {
            return false;
        };
        if sender.send(ComboRunMessage::Event(event)).is_err() {
            guard.remove(run_id);
            return false;
        }
        true
    }

    pub fn send_result(&self, run_id: &str, result: ComboRunResult) -> bool {
        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };
        let Some(sender) = guard.get(run_id) else {
            return false;
        };
        let sent = sender.send(ComboRunMessage::Result(result)).is_ok();
        guard.remove(run_id);
        sent
    }

    pub fn remove(&self, run_id: &str) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.remove(run_id);
        }
    }

    pub fn contains(&self, run_id: &str) -> bool {
        let Ok(guard) = self.inner.lock() else {
            return false;
        };
        guard.contains_key(run_id)
    }
}
