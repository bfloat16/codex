use super::ModelFamily;
use super::model_family;
use pretty_assertions::assert_eq;

#[test]
fn model_family_matches_keywords_anywhere_ignoring_case() {
    let cases = [
        ("DeepSeek-V4.1-Flash", ModelFamily::OpenWeight),
        ("vendor/DEEPSEEK-v4", ModelFamily::OpenWeight),
        ("moonshot/Kimi-K2", ModelFamily::OpenWeight),
        ("prefix-kImI-suffix", ModelFamily::OpenWeight),
        ("zai/GLM-5.3", ModelFamily::OpenWeight),
        ("prefix-glm-suffix", ModelFamily::OpenWeight),
        ("gpt-6-astra", ModelFamily::Gpt),
        ("custom-model", ModelFamily::Gpt),
    ];
    assert_eq!(
        cases.map(|(model, _)| model_family(model)),
        cases.map(|(_, expected)| expected)
    );
}
