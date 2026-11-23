use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("kraai-eval-harness-{}", ulid::Ulid::generate()));
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }

    fn executable(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn resolves_relative_profile_program_and_preserves_argument_boundaries() {
    let fixture = Fixture::new();
    let executable = fixture.executable("agent with spaces", "#!/bin/sh\nexit 0\n");
    let path = fixture.0.join("harness.toml");
    fs::write(
        &path,
        r#"schema_version = 1
name = "custom"
program = "./agent with spaces"
args = ["--model={model}", "{prompt}", "{model}", "{proxy_url}"]
proxy = "openai"
api_key_env = "KRAAI_EVAL_TEST_NONEXISTENT_API_KEY"
"#,
    )
    .unwrap();
    let profile = HarnessProfile::load(&path).unwrap();
    let resolved = profile.resolve("model with spaces", None, None).unwrap();
    assert!(resolved.program == executable.canonicalize().unwrap());
    assert!(resolved.name == "custom");
    assert!(resolved.model_label() == "model with spaces");
    assert!(
        resolved.args
            == [
                "--model=model with spaces",
                "{prompt}",
                "model with spaces",
                "{proxy_url}",
            ]
    );
    assert!(profile.model_proxy(None).unwrap().is_some());
}

#[test]
fn version_is_the_program_sha256() {
    let fixture = Fixture::new();
    let program = fixture.executable("agent", "abc");
    let profile = HarnessProfile::kraai();
    let initial = profile.resolve("test-model", None, Some(&program)).unwrap();
    assert!(initial.version == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    fs::write(&program, "#!/bin/sh\nexit 1\n").unwrap();
    let changed = profile.resolve("test-model", None, Some(&program)).unwrap();
    assert!(initial.version != changed.version);
}

#[test]
fn reasoning_args_come_first_and_only_with_reasoning() {
    let program = std::env::current_exe().unwrap();
    let profile = HarnessProfile::kraai();
    let plain = profile.resolve("test-model", None, Some(&program)).unwrap();
    assert!(plain.args.first().map(String::as_str) == Some("--provider"));
    assert!(plain.model_label() == "test-model");
    let high = profile
        .resolve("test-model", Some("high"), Some(&program))
        .unwrap();
    assert!(
        high.args.get(..3)
            == Some(
                &[
                    String::from("--option"),
                    String::from("reasoning_effort=high"),
                    String::from("--provider"),
                ][..]
            )
    );
    assert!(high.model_label() == "test-model-high");
    let without = HarnessProfile {
        reasoning_args: Vec::new(),
        ..profile
    };
    assert!(
        without
            .resolve("test-model", Some("high"), Some(&program))
            .is_err()
    );
}

#[test]
fn resolves_bare_program_on_path() {
    let profile = HarnessProfile {
        program: PathBuf::from("sh"),
        ..HarnessProfile::kraai()
    };
    let resolved = profile.resolve("test-model", None, None).unwrap();
    assert!(resolved.program.is_absolute());
    assert!(is_executable(&resolved.program));
}

#[test]
fn rejects_invalid_profiles_and_models_before_program_resolution() {
    let profile = HarnessProfile {
        program: std::env::current_exe().unwrap(),
        ..HarnessProfile::kraai()
    };
    for invalid in [
        HarnessProfile {
            schema_version: 2,
            ..profile.clone()
        },
        HarnessProfile {
            name: String::from(" "),
            ..profile.clone()
        },
        HarnessProfile {
            program: PathBuf::new(),
            ..profile.clone()
        },
        HarnessProfile {
            args: vec![String::from("{reasoning}")],
            ..profile.clone()
        },
        HarnessProfile {
            reasoning_args: vec![String::from("--effort")],
            ..profile.clone()
        },
        HarnessProfile {
            max_requests: 0,
            ..profile.clone()
        },
        HarnessProfile {
            proxy: ProxyKind::CodexSubscription,
            ..profile.clone()
        },
        HarnessProfile {
            proxy: ProxyKind::Openai,
            api_key_env: String::from("invalid=key"),
            sanitize_kraai_provider: false,
            ..profile.clone()
        },
    ] {
        assert!(invalid.resolve("test-model", None, None).is_err());
    }
    assert!(profile.resolve(" ", None, None).is_err());
    assert!(profile.resolve("model\0name", None, None).is_err());
    assert!(profile.resolve("model-{prompt}", None, None).is_err());
    assert!(profile.resolve("model", Some("{prompt}"), None).is_err());
}

#[test]
fn rejects_unavailable_runtime_placeholders() {
    let program = std::env::current_exe().unwrap();
    for placeholder in ["{provider_id}", "{provider_config}", "{proxy_url}"] {
        let profile = HarnessProfile {
            program: program.clone(),
            args: vec![format!("--option={placeholder}")],
            proxy: ProxyKind::None,
            sanitize_kraai_provider: false,
            ..HarnessProfile::kraai()
        };
        assert!(profile.resolve("test-model", None, None).is_err());
    }
}

#[test]
fn rejects_non_executable_programs() {
    let fixture = Fixture::new();
    let program = fixture.0.join("agent");
    fs::write(&program, "not executable").unwrap();
    let profile = HarnessProfile::kraai();
    assert!(
        profile
            .resolve("test-model", None, Some(&fixture.0))
            .is_err()
    );
    #[cfg(unix)]
    assert!(profile.resolve("test-model", None, Some(&program)).is_err());
    assert!(
        profile
            .resolve("test-model", None, Some(&fixture.0.join("absent")))
            .is_err()
    );
}

#[test]
fn example_profile_matches_builtin() {
    let example =
        HarnessProfile::load(&crate::eval_assets_directory().join("harnesses/kraai.toml")).unwrap();
    assert!(
        serde_json::to_value(&example).unwrap()
            == serde_json::to_value(HarnessProfile::kraai()).unwrap()
    );
    assert!(example.model_proxy(None).is_err());
}
