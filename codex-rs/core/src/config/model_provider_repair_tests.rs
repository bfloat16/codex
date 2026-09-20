use super::*;
use pretty_assertions::assert_eq;

#[test]
fn gpt_model_uses_first_configured_non_deepseek_provider() {
    assert_eq!(
        correction(
            Some("gpt-5.6-luna"),
            "deepseek",
            vec![
                "zeta".to_string(),
                "deepseek".to_string(),
                "anyrouter".to_string(),
            ],
        ),
        Ok(Some(ModelProviderCorrection {
            model: "gpt-5.6-luna".to_string(),
            model_provider: "anyrouter".to_string(),
        }))
    );
}

#[test]
fn deepseek_provider_without_model_selects_default_deepseek_model() {
    assert_eq!(
        correction(
            /*model*/ None,
            "deepseek",
            vec!["deepseek".to_string()],
        ),
        Ok(Some(ModelProviderCorrection {
            model: "deepseek-flash".to_string(),
            model_provider: "deepseek".to_string(),
        }))
    );
}
