use super::*;

#[test]
fn pricing_catalog_preserves_endpoint_defaults_and_overrides() {
    for (policy, default_api) in [
        (GenericChatCompletionsProfile::pricing_policy(), None),
        (
            OpenAiChatCompletionsProfile::pricing_policy(),
            Some("https://api.openai.com/v1"),
        ),
    ] {
        assert!(!policy.subscription);
        let default_catalog = (policy.catalog)(&DynamicConfig::new());
        assert_eq!(default_catalog.api.as_deref(), default_api);
        assert_eq!(
            default_catalog.provider.as_deref(),
            default_api.map(|_| "openai")
        );
        for (base_url, expected_provider) in [
            ("https://api.openai.com/v1", Some("openai")),
            ("  https://api.openai.com/v1///  ", Some("openai")),
            ("https://proxy.test/v1", None),
            ("  ", None),
        ] {
            let catalog = (policy.catalog)(&DynamicConfig::from([(
                "base_url".into(),
                DynamicValue::from(base_url),
            )]));
            assert_eq!(catalog.api.as_deref(), Some(base_url.trim()));
            assert_eq!(catalog.provider.as_deref(), expected_provider);
        }
        let invalid_catalog = (policy.catalog)(&DynamicConfig::from([(
            "base_url".into(),
            DynamicValue::Bool(false),
        )]));
        assert_eq!(invalid_catalog, default_catalog);
        let explicit = (policy.catalog)(&DynamicConfig::from([
            (
                "base_url".into(),
                DynamicValue::from("https://api.openai.com/v1"),
            ),
            ("catalog_provider".into(), DynamicValue::from("  custom  ")),
        ]));
        assert_eq!(explicit.provider.as_deref(), Some("custom"));
    }
}
