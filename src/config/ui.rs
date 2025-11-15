#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct UI {
    pub markdown_render_engine: MarkdownRenderEngine,
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
