//! Provider selection and model resolution utilities.
//!
//! This module provides functions for selecting the appropriate AI provider
//! based on model configuration and resolving model names.

use crate::{Config, ProviderConfig, Result};

/// Select the appropriate provider index for the given model.
///
/// # Selection Algorithm
/// 1. If no model is specified, returns the first provider
/// 2. If a model is specified, returns the first provider that:
///    - Matches provider name, OR
///    - Has the requested model in its models list
/// 3. If no exact match, returns the first wildcard provider
/// 4. If no matching provider, returns an error
pub fn select_provider_index(
    agent_model: Option<&str>,
    providers: &[ProviderConfig],
) -> Result<usize> {
    if providers.is_empty() {
        snafu::whatever!("No providers configured")
    }

    // If no model specified, use first provider
    let Some(model) = agent_model else {
        return Ok(0);
    };

    // Exact match by provider name or declared models
    if let Some((idx, _)) = providers.iter().enumerate().find(|(_, provider)| {
        provider.name == model
            || provider
                .models
                .as_ref()
                .is_some_and(|models| models.iter().any(|candidate| candidate == model))
    }) {
        return Ok(idx);
    }

    // Wildcard provider (no models specified or empty list)
    if let Some((idx, _)) = providers.iter().enumerate().find(|(_, provider)| {
        provider.models.is_none() || provider.models.as_ref().is_some_and(|m| m.is_empty())
    }) {
        return Ok(idx);
    }

    let available: Vec<_> = providers
        .iter()
        .map(|p| (p.name.clone(), p.models.clone()))
        .collect();
    snafu::whatever!(
        "No provider supports model '{}'. Available providers: {:?}",
        model,
        available
    )
}

/// Resolve the model name from a provider and optional selected model.
pub fn resolve_model(provider: &ProviderConfig, selected_model: Option<String>) -> String {
    selected_model.unwrap_or_else(|| {
        provider
            .models
            .as_ref()
            .and_then(|models| models.first().cloned())
            .unwrap_or_else(|| provider.name.to_owned())
    })
}

/// Get the current model name from an agent's configuration.
pub fn get_current_model(
    default_model: Option<&str>,
    model_override: Option<&str>,
    providers: &[ProviderConfig],
) -> String {
    let selected_model = model_override.or(default_model).map(|s| s.to_string());
    match select_provider_index(selected_model.as_deref(), providers) {
        Ok(idx) => {
            let provider = &providers[idx];
            resolve_model(provider, selected_model)
        }
        Err(_) => selected_model.unwrap_or_else(|| "unknown".to_string()),
    }
}

/// Get the resolved default model name from an agent's configuration.
pub fn get_resolved_default_model(
    default_model: Option<&str>,
    providers: &[ProviderConfig],
) -> String {
    let default_model = default_model.map(|s| s.to_string());
    match select_provider_index(default_model.as_deref(), providers) {
        Ok(idx) => {
            let provider = &providers[idx];
            resolve_model(provider, default_model)
        }
        Err(_) => default_model.unwrap_or_else(|| "unknown".to_string()),
    }
}

/// Check if the current provider has offload_combo_reply enabled.
pub fn has_offload_combo_reply(
    default_model: Option<&str>,
    model_override: Option<&str>,
    providers: &[ProviderConfig],
    config: &Config,
) -> bool {
    let selected_model = model_override.or(default_model);
    match select_provider_index(selected_model, providers) {
        Ok(idx) => {
            let provider = &providers[idx];
            let model = resolve_model(provider, selected_model.map(|s| s.to_string()));
            let mut options = config.request_options_for_model(&model);
            options.apply_override(&provider.request_overrides);
            options.offload_combo_reply.unwrap_or(false)
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderKind;

    fn create_test_provider(name: &str, models: Option<Vec<String>>) -> ProviderConfig {
        ProviderConfig {
            name: name.to_string(),
            kind: ProviderKind::OpenAI,
            api_key: crate::EnvString::String("test".to_string()),
            base_url: "http://localhost".to_string(),
            models,
            request_overrides: Default::default(),
        }
    }

    #[test]
    fn select_provider_index_with_no_model_returns_first() {
        let providers = vec![
            create_test_provider("provider1", Some(vec!["model1".to_string()])),
            create_test_provider("provider2", Some(vec!["model2".to_string()])),
        ];
        let idx = select_provider_index(None, &providers).unwrap();
        assert_eq!(idx, 0);
    }

    #[test]
    fn select_provider_index_with_exact_match() {
        let providers = vec![
            create_test_provider("provider1", Some(vec!["model1".to_string()])),
            create_test_provider("provider2", Some(vec!["model2".to_string()])),
        ];
        let idx = select_provider_index(Some("model2"), &providers).unwrap();
        assert_eq!(idx, 1);
    }

    #[test]
    fn select_provider_index_with_provider_name() {
        let providers = vec![
            create_test_provider("provider1", Some(vec!["model1".to_string()])),
            create_test_provider("provider2", Some(vec!["model2".to_string()])),
        ];
        let idx = select_provider_index(Some("provider1"), &providers).unwrap();
        assert_eq!(idx, 0);
    }

    #[test]
    fn select_provider_index_with_wildcard() {
        let providers = vec![
            create_test_provider("provider1", Some(vec!["model1".to_string()])),
            create_test_provider("wildcard", None),
        ];
        let idx = select_provider_index(Some("unknown_model"), &providers).unwrap();
        assert_eq!(idx, 1);
    }

    #[test]
    fn select_provider_index_with_empty_models_list() {
        let providers = vec![
            create_test_provider("provider1", Some(vec!["model1".to_string()])),
            create_test_provider("wildcard", Some(vec![])),
        ];
        let idx = select_provider_index(Some("unknown_model"), &providers).unwrap();
        assert_eq!(idx, 1);
    }

    #[test]
    fn select_provider_index_with_no_match_returns_error() {
        let providers = vec![
            create_test_provider("provider1", Some(vec!["model1".to_string()])),
            create_test_provider("provider2", Some(vec!["model2".to_string()])),
        ];
        let result = select_provider_index(Some("unknown_model"), &providers);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_model_with_selected_uses_selected() {
        let provider = create_test_provider("provider1", Some(vec!["model1".to_string()]));
        let model = resolve_model(&provider, Some("custom_model".to_string()));
        assert_eq!(model, "custom_model");
    }

    #[test]
    fn resolve_model_with_none_uses_first_from_list() {
        let provider = create_test_provider(
            "provider1",
            Some(vec!["model1".to_string(), "model2".to_string()]),
        );
        let model = resolve_model(&provider, None);
        assert_eq!(model, "model1");
    }

    #[test]
    fn resolve_model_with_empty_list_uses_provider_name() {
        let provider = create_test_provider("provider1", Some(vec![]));
        let model = resolve_model(&provider, None);
        assert_eq!(model, "provider1");
    }
}
