use std::sync::OnceLock;

use serde::Deserialize;

use super::provider::ModelPreset;

#[derive(Debug, Deserialize)]
struct PresetFile {
    #[serde(default)]
    model_presets: Vec<ModelPreset>,
}

pub(crate) fn builtin_model_presets() -> &'static [ModelPreset] {
    static PRESETS: OnceLock<Vec<ModelPreset>> = OnceLock::new();
    PRESETS
        .get_or_init(|| {
            let content = include_str!("presets.toml");
            let parsed: PresetFile =
                toml::from_str(content).expect("failed to parse builtin presets");
            parsed.model_presets
        })
        .as_slice()
}

#[cfg(test)]
mod tests {
    use super::builtin_model_presets;

    #[test]
    fn builtin_presets_are_cached_once() {
        let first = builtin_model_presets();
        let second = builtin_model_presets();
        assert_eq!(first.as_ptr(), second.as_ptr());
    }
}
