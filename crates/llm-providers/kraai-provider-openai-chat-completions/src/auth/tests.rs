use super::*;

fn authorization(config: DynamicConfig) -> Option<String> {
    ApiKeyAuth::resolve(&config)
        .apply(reqwest::Client::new().get("http://127.0.0.1/v1/models"))
        .build()
        .unwrap()
        .headers()
        .get(reqwest::header::AUTHORIZATION)
        .map(|value| value.to_str().unwrap().to_string())
}

#[test]
fn requests_carry_the_key_only_when_one_is_configured() {
    assert_eq!(
        authorization(DynamicConfig::from([(
            "api_key".into(),
            DynamicValue::from("fixture-key")
        )])),
        Some(String::from("Bearer fixture-key"))
    );
    for config in [
        DynamicConfig::new(),
        DynamicConfig::from([("api_key".into(), DynamicValue::from("  "))]),
        DynamicConfig::from([(
            "env_var_api_key".into(),
            DynamicValue::from("KRAAI_TEST_UNSET_API_KEY_VARIABLE"),
        )]),
    ] {
        assert_eq!(authorization(config), None);
    }
}
