use std::time::Duration;

use code_combo::IdleNotification;
use tokio::time::Instant;
use tracing::debug;

#[derive(Debug, Clone)]
pub struct IdleTracker {
    last_interaction: Instant,
    last_notification: Option<Instant>,
    notifications_sent: u32,
}

impl IdleTracker {
    pub fn new(now: Instant) -> Self {
        Self {
            last_interaction: now,
            last_notification: None,
            notifications_sent: 0,
        }
    }

    pub fn record_activity(&mut self, now: Instant) {
        self.last_interaction = now;
        if self.notifications_sent > 0 || self.last_notification.is_some() {
            debug!(
                sent = self.notifications_sent,
                "idle tracker reset on activity"
            );
            self.notifications_sent = 0;
            self.last_notification = None;
        }
    }

    pub fn poll(
        &mut self,
        now: Instant,
        config: &IdleNotification,
        should_notify: bool,
    ) -> Option<u32> {
        if !config.enabled || !should_notify {
            return None;
        }
        if config.max_notifications == 0 {
            return None;
        }

        let idle_for = now.duration_since(self.last_interaction);
        let timeout = Duration::from_secs(config.timeout_seconds);
        if idle_for < timeout {
            return None;
        }

        if self.notifications_sent >= config.max_notifications {
            return None;
        }

        if let Some(last) = self.last_notification {
            let interval = Duration::from_secs(config.interval_seconds);
            if now.duration_since(last) < interval {
                return None;
            }
        }

        self.notifications_sent += 1;
        self.last_notification = Some(now);
        debug!(
            idle_secs = idle_for.as_secs(),
            sent = self.notifications_sent,
            "idle notification triggered"
        );
        Some(self.notifications_sent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_combo::NotificationWhen;

    fn config() -> IdleNotification {
        IdleNotification {
            enabled: true,
            when: NotificationWhen::Always,
            timeout_seconds: 5,
            max_notifications: 3,
            interval_seconds: 2,
        }
    }

    #[test]
    fn idle_triggers_after_timeout() {
        let start = Instant::now();
        let mut tracker = IdleTracker::new(start);
        let config = config();

        assert_eq!(
            tracker.poll(start + Duration::from_secs(4), &config, true),
            None
        );
        assert_eq!(
            tracker.poll(start + Duration::from_secs(5), &config, true),
            Some(1)
        );
    }

    #[test]
    fn idle_limits_notifications() {
        let start = Instant::now();
        let mut tracker = IdleTracker::new(start);
        let config = IdleNotification {
            enabled: true,
            when: NotificationWhen::Always,
            timeout_seconds: 0,
            max_notifications: 2,
            interval_seconds: 1,
        };

        assert_eq!(tracker.poll(start, &config, true), Some(1));
        assert_eq!(
            tracker.poll(start + Duration::from_secs(1), &config, true),
            Some(2)
        );
        assert_eq!(
            tracker.poll(start + Duration::from_secs(2), &config, true),
            None
        );
    }

    #[test]
    fn idle_resets_on_activity() {
        let start = Instant::now();
        let mut tracker = IdleTracker::new(start);
        let config = config();

        assert_eq!(
            tracker.poll(start + Duration::from_secs(5), &config, true),
            Some(1)
        );

        tracker.record_activity(start + Duration::from_secs(6));

        assert_eq!(
            tracker.poll(start + Duration::from_secs(9), &config, true),
            None
        );
        assert_eq!(
            tracker.poll(start + Duration::from_secs(11), &config, true),
            Some(1)
        );
    }
}
