#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct UI {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub markdown_render_engine: MarkdownRenderEngine,
}

fn default_theme() -> String {
    "catppuccin_mocha".to_string()
}

impl Default for UI {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            markdown_render_engine: MarkdownRenderEngine::default(),
        }
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum MarkdownRenderEngine {
    #[default]
    Native,
    ExternalCommand {
        executable: String,
        args: Vec<String>,
    },
}
