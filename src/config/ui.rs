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
    #[serde(default = "default_notifications_only_when_unfocused")]
    pub only_when_unfocused: bool,
    #[serde(default)]
    pub backend: NotificationBackend,
}

fn default_notifications_enabled() -> bool {
    true
}

fn default_notifications_only_when_unfocused() -> bool {
    true
}

impl Default for UINotifications {
    fn default() -> Self {
        Self {
            enabled: default_notifications_enabled(),
            only_when_unfocused: default_notifications_only_when_unfocused(),
            backend: NotificationBackend::default(),
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
