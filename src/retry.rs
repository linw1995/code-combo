use std::{fmt, sync::Arc, time::Duration};

#[derive(Debug, Clone)]
pub struct RetryAttempt {
    pub attempt: usize,
    pub max_attempts: usize,
    pub delay: Duration,
    pub error: String,
}

#[derive(Debug, Clone)]
pub enum RetryUpdate {
    Attempt(RetryAttempt),
    Finished { success: bool },
}

#[derive(Clone)]
pub struct RetryNotifier(Arc<dyn Fn(RetryUpdate) + Send + Sync>);

impl RetryNotifier {
    pub fn new<F>(notifier: F) -> Self
    where
        F: Fn(RetryUpdate) + Send + Sync + 'static,
    {
        Self(Arc::new(notifier))
    }

    pub fn notify(&self, update: RetryUpdate) {
        (self.0)(update);
    }
}

impl fmt::Debug for RetryNotifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RetryNotifier(..)")
    }
}
