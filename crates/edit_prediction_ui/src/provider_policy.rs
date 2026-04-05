use gpui::App;
use language::language_settings::{EditPredictionProvider, all_language_settings};

pub fn current_edit_prediction_provider(cx: &App) -> EditPredictionProvider {
    normalize_edit_prediction_provider(all_language_settings(None, cx).edit_predictions.provider)
}

pub fn normalize_edit_prediction_provider(
    provider: EditPredictionProvider,
) -> EditPredictionProvider {
    if edit_prediction_provider_supported(provider) {
        provider
    } else {
        EditPredictionProvider::None
    }
}

pub fn edit_prediction_provider_supported(provider: EditPredictionProvider) -> bool {
    match provider {
        EditPredictionProvider::Zed | EditPredictionProvider::Experimental(_) => {
            zed_hosted_provider_supported()
        }
        EditPredictionProvider::None
        | EditPredictionProvider::Copilot
        | EditPredictionProvider::Codestral
        | EditPredictionProvider::Ollama
        | EditPredictionProvider::OpenAiCompatibleApi
        | EditPredictionProvider::Sweep
        | EditPredictionProvider::Mercury => true,
    }
}

pub fn supported_edit_prediction_providers() -> Vec<EditPredictionProvider> {
    let mut providers = vec![EditPredictionProvider::None];

    if zed_hosted_provider_supported() {
        providers.push(EditPredictionProvider::Zed);
    }

    providers.extend([
        EditPredictionProvider::Copilot,
        EditPredictionProvider::Codestral,
        EditPredictionProvider::Ollama,
        EditPredictionProvider::OpenAiCompatibleApi,
        EditPredictionProvider::Sweep,
        EditPredictionProvider::Mercury,
    ]);

    providers
}

pub const fn zed_hosted_provider_supported() -> bool {
    cfg!(feature = "zed-hosted-provider")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_edit_prediction_provider_disables_zed_hosted_providers_by_default() {
        assert_eq!(
            normalize_edit_prediction_provider(EditPredictionProvider::Zed),
            EditPredictionProvider::None
        );
        assert_eq!(
            normalize_edit_prediction_provider(EditPredictionProvider::Experimental("zeta2")),
            EditPredictionProvider::None
        );
        assert_eq!(
            normalize_edit_prediction_provider(EditPredictionProvider::Copilot),
            EditPredictionProvider::Copilot
        );
        assert_eq!(
            normalize_edit_prediction_provider(EditPredictionProvider::Codestral),
            EditPredictionProvider::Codestral
        );
        assert_eq!(
            normalize_edit_prediction_provider(EditPredictionProvider::Mercury),
            EditPredictionProvider::Mercury
        );
    }
}
