#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct UI {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub markdown_render_engine: MarkdownRenderEngine,
    #[serde(default)]
    pub notifications: UINotifications,
}

fn default_theme() -> String {
    "catppuccin_mocha".to_string()
}

impl Default for UI {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            markdown_render_engine: MarkdownRenderEngine::default(),
            notifications: UINotifications::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct UINotifications {
    #[serde(default = "default_notifications_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub reply_ready: NotificationRule,
    #[serde(default)]
    pub action_required: NotificationRule,
    #[serde(default)]
    pub idle: IdleNotification,
    #[serde(default)]
    pub backend: NotificationBackend,
}

fn default_notifications_enabled() -> bool {
    true
}

impl Default for UINotifications {
    fn default() -> Self {
        Self {
            enabled: default_notifications_enabled(),
            reply_ready: NotificationRule::default(),
            action_required: NotificationRule::default(),
            idle: IdleNotification::default(),
            backend: NotificationBackend::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationWhen {
    Focused,
    Unfocused,
    Always,
}

fn default_notification_when_unfocused() -> NotificationWhen {
    NotificationWhen::Unfocused
}

fn default_notification_when_always() -> NotificationWhen {
    NotificationWhen::Always
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct NotificationRule {
    #[serde(default = "default_notification_rule_enabled")]
    pub enabled: bool,
    #[serde(default = "default_notification_when_unfocused")]
    pub when: NotificationWhen,
}

fn default_notification_rule_enabled() -> bool {
    true
}

impl Default for NotificationRule {
    fn default() -> Self {
        Self {
            enabled: default_notification_rule_enabled(),
            when: default_notification_when_unfocused(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct IdleNotification {
    #[serde(default = "default_idle_notification_enabled")]
    pub enabled: bool,
    #[serde(default = "default_notification_when_always")]
    pub when: NotificationWhen,
    #[serde(default = "default_idle_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_idle_max_notifications")]
    pub max_notifications: u32,
    #[serde(default = "default_idle_notification_interval_seconds")]
    pub interval_seconds: u64,
}

fn default_idle_notification_enabled() -> bool {
    true
}

fn default_idle_timeout_seconds() -> u64 {
    300
}

fn default_idle_max_notifications() -> u32 {
    1
}

fn default_idle_notification_interval_seconds() -> u64 {
    60
}

impl Default for IdleNotification {
    fn default() -> Self {
        Self {
            enabled: default_idle_notification_enabled(),
            when: default_notification_when_always(),
            timeout_seconds: default_idle_timeout_seconds(),
            max_notifications: default_idle_max_notifications(),
            interval_seconds: default_idle_notification_interval_seconds(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotificationBackend {
    #[default]
    Osc9,
    ExternalCommand {
        executable: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum MarkdownRenderEngine {
    #[default]
    Native,
    ExternalCommand {
        executable: String,
        args: Vec<String>,
    },
}
