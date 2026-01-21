use std::sync::OnceLock;

use serde::Deserialize;

use super::provider::ModelPreset;

#[derive(Debug, Deserialize)]
struct PresetFile {
    #[serde(default)]
    model_presets: Vec<ModelPreset>,
}

pub(crate) fn builtin_model_presets() -> Vec<ModelPreset> {
    static PRESETS: OnceLock<Vec<ModelPreset>> = OnceLock::new();
    PRESETS
        .get_or_init(|| {
            let content = include_str!("presets.toml");
            let parsed: PresetFile =
                toml::from_str(content).expect("failed to parse builtin presets");
            parsed.model_presets
        })
        .clone()
}
