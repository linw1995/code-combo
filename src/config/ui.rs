#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct UI {
    #[serde(default = "default_colorscheme")]
    pub colorschema: String,
    pub markdown_render_engine: MarkdownRenderEngine,
}

fn default_colorscheme() -> String {
    "catppuccin_mocha".to_string()
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
