use super::*;

fn config(entries: &[(&str, &str)]) -> DynamicConfig {
    entries
        .iter()
        .map(|(key, value)| (String::from(*key), DynamicValue::from(*value)))
        .collect()
}

fn authorization(config: DynamicConfig) -> Option<String> {
    ApiKeyAuth::resolve(&config)
        .unwrap()
        .apply(reqwest::Client::new().get("http://127.0.0.1/v1/models"))
        .build()
        .unwrap()
        .headers()
        .get(reqwest::header::AUTHORIZATION)
        .map(|value| value.to_str().unwrap().to_string())
}

#[test]
fn inline_key_wins_over_the_environment_variable() {
    assert_eq!(
        authorization(config(&[
            ("api_key", "fixture-key"),
            ("env_var_api_key", "KRAAI_TEST_UNSET_API_KEY_VARIABLE"),
        ])),
        Some(String::from("Bearer fixture-key"))
    );
}

#[test]
fn environment_variable_supplies_the_key() {
    assert_eq!(
        ApiKeyAuth::resolve(&config(&[("env_var_api_key", "PATH")]))
            .unwrap()
            .api_key,
        Some(std::env::var("PATH").unwrap())
    );
}

#[test]
fn missing_environment_variable_fails_and_names_it() {
    for entries in [
        &[("env_var_api_key", "KRAAI_TEST_UNSET_API_KEY_VARIABLE")][..],
        &[
            ("api_key", "  "),
            ("env_var_api_key", " KRAAI_TEST_UNSET_API_KEY_VARIABLE "),
        ][..],
    ] {
        let error = ApiKeyAuth::resolve(&config(entries)).err().unwrap();
        let message = error.to_string();
        assert!(
            message.contains("KRAAI_TEST_UNSET_API_KEY_VARIABLE"),
            "{message}"
        );
        assert!(message.contains("Env Var"), "{message}");
    }
}

#[test]
fn no_key_and_no_environment_variable_sends_no_authorization() {
    for entries in [
        &[][..],
        &[("api_key", "  ")][..],
        &[("env_var_api_key", "  ")][..],
    ] {
        assert_eq!(authorization(config(entries)), None);
    }
}
