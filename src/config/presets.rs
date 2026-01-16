use std::sync::OnceLock;

use serde::Deserialize;

use super::provider::ModelRequestConfig;

#[derive(Debug, Deserialize)]
struct PresetFile {
    #[serde(default)]
    model_presets: Vec<ModelRequestConfig>,
}

pub(crate) fn builtin_model_presets() -> Vec<ModelRequestConfig> {
    static PRESETS: OnceLock<Vec<ModelRequestConfig>> = OnceLock::new();
    PRESETS
        .get_or_init(|| {
            let content = include_str!("presets.toml");
            let parsed: PresetFile =
                toml::from_str(content).expect("failed to parse builtin presets");
            parsed.model_presets
        })
        .clone()
}
