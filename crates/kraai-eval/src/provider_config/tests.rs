use super::*;

#[test]
fn prepared_config_keeps_identity_and_materialization_on_one_snapshot() {
    let root = std::env::temp_dir().join(format!(
        "kraai-eval-provider-snapshot-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir(&root).unwrap();
    let source = root.join("providers.toml");
    fs::write(
        &source,
        "[[provider]]\nid = 'original'\ntype = 'openai-codex'\nbase_url = 'https://original.invalid'\n[[model]]\nid = 'model'\nprovider_id = 'original'\nname = ' Original Model '\n",
    ).unwrap();
    let request = KraaiProviderConfigRequest::new(source.clone(), None);
    let prepared = request.prepare().unwrap();
    let digest = prepared.digest().unwrap();
    let expected: ProviderManagerConfig = toml::from_str(
        "[[provider]]\nid = 'original'\ntype = 'openai-codex'\nbase_url = 'http://eval-proxy.invalid/backend-api'\nproxy_token_env = 'KRAAI_EVAL_CODEX_PROXY_TOKEN'\n[[model]]\nid = 'model'\nprovider_id = 'original'\nname = ' Original Model '\n",
    ).unwrap();
    assert!(
        digest
            == crate::cache::hash_chunks(&[
                toml::to_string(&expected).unwrap().into_bytes(),
                EVAL_AGENT_PROFILES.as_bytes().to_vec(),
            ])
    );

    fs::write(
        &source,
        "[[provider]]\nid = 'replacement'\ntype = 'openai-codex'\n",
    )
    .unwrap();
    let path = prepared
        .materialize(&root, "http://127.0.0.1:1234/backend-api", None)
        .unwrap();
    let materialized: ProviderManagerConfig = toml::from_slice(&fs::read(path).unwrap()).unwrap();
    assert!(prepared.selected_provider_id() == "original");
    assert!(prepared.digest().unwrap() == digest);
    assert!(
        materialized
            .providers
            .first()
            .is_some_and(|provider| provider.id.as_str() == "original")
    );
    assert!(materialized.models == expected.models);
    assert!(request.prepare().unwrap().digest().unwrap() != digest);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sanitizer_keeps_only_selected_codex_provider_and_models() {
    let root = std::env::temp_dir().join(format!(
        "kraai-eval-provider-config-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir(&root).unwrap();
    let source = root.join("providers.toml");
    fs::write(
        &source,
        r#"
[[provider]]
id = "codex-main"
type = "openai-codex"
base_url = "https://should-be-overridden.invalid"

[[provider]]
id = "api-key-provider"
type = "openai-chat-completions"
api_key = "must-not-leak"
base_url = "https://api.openai.com/v1"

[[model]]
id = "gpt-5.6-high"
provider_id = "codex-main"
name = "GPT-5.6 High"

[[model]]
id = "secret-model"
provider_id = "api-key-provider"
"#,
    )
    .unwrap();
    let request = KraaiProviderConfigRequest::new(source, Some(String::from("codex-main")))
        .prepare()
        .unwrap();
    assert!(request.selected_provider_id() == "codex-main");
    let workspace = root.join("workspace");
    fs::create_dir(&workspace).unwrap();
    let output = request
        .materialize(&workspace, "http://127.0.0.1:1234/backend-api", None)
        .unwrap();
    let rendered = fs::read_to_string(output).unwrap();
    assert!(
        rendered.contains("codex-main"),
        "selected provider was removed"
    );
    assert!(
        rendered.contains("gpt-5.6-high"),
        "selected model was removed"
    );
    assert!(
        rendered.contains("KRAAI_EVAL_CODEX_PROXY_TOKEN"),
        "proxy token environment was not configured"
    );
    assert!(
        rendered.contains("http://127.0.0.1:1234/backend-api"),
        "proxy URL was not configured"
    );
    assert!(!rendered.contains("must-not-leak"), "API key leaked");
    assert!(
        !rendered.contains("secret-model"),
        "unselected model leaked"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sanitizer_routes_chat_completions_provider_through_proxy_token() {
    let root = std::env::temp_dir().join(format!(
        "kraai-eval-chat-provider-config-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir(&root).unwrap();
    let source = root.join("providers.toml");
    fs::write(
        &source,
        r#"
[[provider]]
id = "codex-main"
type = "openai-codex"

[[provider]]
id = "local"
type = "openai-chat-completions"
api_key = "must-not-leak"
base_url = "http://127.0.0.1:10014/v1"
only_listed_models = false
custom_header = "must-not-leak-either"

[[model]]
id = "local-model"
provider_id = "local"
"#,
    )
    .unwrap();
    assert!(
        KraaiProviderConfigRequest::new(source.clone(), None)
            .kind()
            .is_err_and(|error| error.to_string().contains("found: codex-main, local"))
    );
    let request = KraaiProviderConfigRequest::new(source, Some(String::from("local")));
    assert!(request.kind().unwrap() == KraaiProviderKind::ChatCompletions);
    let upstream = request.chat_completions_upstream().unwrap();
    assert!(upstream.provider_id == "local");
    assert!(upstream.base_url == "http://127.0.0.1:10014/v1");
    assert!(upstream.api_key == "must-not-leak");

    let path = request
        .prepare()
        .unwrap()
        .materialize(&root, "http://127.0.0.1:1234/v1", None)
        .unwrap();
    let materialized: ProviderManagerConfig = toml::from_slice(&fs::read(path).unwrap()).unwrap();
    let expected: ProviderManagerConfig = toml::from_str(
        "[[provider]]\nid = 'local'\ntype = 'openai-chat-completions'\nbase_url = 'http://127.0.0.1:1234/v1'\ncache_breakpoints = false\nenv_var_api_key = 'OPENAI_API_KEY'\nonly_listed_models = false\nprompt_cache_key = false\n[[model]]\nid = 'local-model'\nprovider_id = 'local'\n",
    ).unwrap();
    assert!(toml::to_string(&materialized).unwrap() == toml::to_string(&expected).unwrap());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn chat_completions_cache_hints_survive_the_proxy_base_url() {
    let root = std::env::temp_dir().join(format!(
        "kraai-eval-chat-cache-hints-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir(&root).unwrap();
    let source = root.join("providers.toml");
    fs::write(
        &source,
        r#"
[[provider]]
id = "openrouter"
type = "openai-chat-completions"
api_key = "key"
base_url = "https://openrouter.ai/api/v1"
prompt_cache_key = true
"#,
    )
    .unwrap();
    let path = KraaiProviderConfigRequest::new(source, None)
        .prepare()
        .unwrap()
        .materialize(&root, "http://127.0.0.1:1234/v1", None)
        .unwrap();
    let materialized: ProviderManagerConfig = toml::from_slice(&fs::read(path).unwrap()).unwrap();
    let provider = materialized
        .providers
        .first()
        .expect("provider was removed");
    for key in ["cache_breakpoints", "prompt_cache_key"] {
        assert!(
            provider.config.get(key) == Some(&DynamicValue::Bool(true)),
            "{key} was not kept"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_context_limit_caps_the_evaluated_model() {
    let root = std::env::temp_dir().join(format!(
        "kraai-eval-context-limit-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir(&root).unwrap();
    let source = root.join("providers.toml");
    fs::write(
        &source,
        "[[provider]]\nid = 'local'\ntype = 'openai-chat-completions'\napi_key = 'key'\nbase_url = 'http://127.0.0.1:10014/v1'\n[[model]]\nid = 'small'\nprovider_id = 'local'\nmax_context = 8000\n",
    ).unwrap();
    let limit = |model: &str, max_context| -> Result<Option<i64>> {
        let path = KraaiProviderConfigRequest::new(source.clone(), None)
            .with_model(model)
            .prepare()?
            .materialize(&root, "http://127.0.0.1:1234/v1", Some(max_context))?;
        let config: ProviderManagerConfig = toml::from_slice(&fs::read(path)?)?;
        Ok(config
            .models
            .iter()
            .find(|entry| entry.id.as_str() == model)
            .and_then(|entry| entry.config.get(MAX_CONTEXT_KEY))
            .and_then(DynamicValue::as_integer))
    };
    assert!(limit("small", 4000).unwrap() == Some(4000));
    assert!(limit("small", 16000).unwrap() == Some(8000));
    assert!(limit("discovered", 24000).unwrap() == Some(24000));
    assert!(
        KraaiProviderConfigRequest::new(source, None)
            .prepare()
            .unwrap()
            .materialize(&root, "http://127.0.0.1:1234/v1", Some(4000))
            .is_err()
    );
    fs::remove_dir_all(root).unwrap();
}
