use std::fs;

use super::*;

#[test]
fn chat_completions_credentials_split_the_provider_base_url() {
    let root = std::env::temp_dir().join(format!(
        "kraai-eval-chat-credentials-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir(&root).unwrap();
    let source = root.join("providers.toml");
    for (base_url, upstream, base_path) in [
        (
            "http://127.0.0.1:10014/v1/",
            "http://127.0.0.1:10014",
            "/v1",
        ),
        ("https://example.invalid", "https://example.invalid", ""),
    ] {
        fs::write(
            &source,
            format!(
                "[[provider]]\nid = 'local'\ntype = 'openai-chat-completions'\napi_key = 'key'\nbase_url = '{base_url}'\n"
            ),
        ).unwrap();
        let credentials = ProxyCredentialRequest::ChatCompletions {
            provider: KraaiProviderConfigRequest::new(source.clone(), None),
        }
        .resolve()
        .unwrap();
        assert!(credentials.upstream() == upstream);
        assert!(credentials.base_path() == base_path);
        assert!(
            credentials.allowed_paths()
                == BTreeSet::from([
                    format!("{base_path}/chat/completions"),
                    format!("{base_path}/models"),
                ])
        );
        assert!(credentials.credential_source() == "kraai-provider:local");
    }
    for base_url in [
        "http://127.0.0.1/v1?x=1",
        "http://user:secret@127.0.0.1/v1",
        "http://user@127.0.0.1/v1",
    ] {
        fs::write(
            &source,
            format!(
                "[[provider]]\nid = 'local'\ntype = 'openai-chat-completions'\napi_key = 'key'\nbase_url = '{base_url}'\n"
            ),
        ).unwrap();
        assert!(
            ProxyCredentialRequest::ChatCompletions {
                provider: KraaiProviderConfigRequest::new(source.clone(), None),
            }
            .resolve()
            .is_err()
        );
    }
    fs::remove_dir_all(root).unwrap();
}
