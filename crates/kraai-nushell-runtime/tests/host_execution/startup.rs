use super::*;

#[tokio::test]
async fn inherited_startup_evaluates_env_and_config_files_before_the_script() {
    let workspace = TestWorkspace::new();
    let config_home = workspace.0.join("config");
    let nushell_config = config_home.join("nushell");
    std::fs::create_dir_all(&nushell_config)
        .unwrap_or_else(|error| panic!("unable to create Nushell config fixture: {error}"));
    std::fs::write(
        nushell_config.join("env.nu"),
        "$env.KRAAI_ENV_STARTUP = 'env-loaded'\n",
    )
    .unwrap_or_else(|error| panic!("unable to write env.nu fixture: {error}"));
    std::fs::write(
        nushell_config.join("config.nu"),
        "$env.config.history.file_format = 'sqlite'\n$env.config.history.isolation = false\n$env.KRAAI_CONFIG_STARTUP = 'config-loaded'\n",
    )
    .unwrap_or_else(|error| panic!("unable to write config.nu fixture: {error}"));

    let mut execution = plan(
        b"[$env.KRAAI_ENV_STARTUP $env.KRAAI_CONFIG_STARTUP $env.config.history.file_format] | to json --raw".to_vec(),
        &workspace,
    );
    execution.nushell_startup = NushellStartup::Inherit;
    execution.environment.insert(
        String::from("XDG_CONFIG_HOME"),
        config_home.display().to_string(),
    );

    let result = execute(execution, CancellationToken::new())
        .await
        .unwrap_or_else(|error| panic!("host execution failed: {error}"));
    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) },
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.output.stdout),
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&result.output.stdout),
        "[\"env-loaded\",\"config-loaded\",\"plaintext\"]\n"
    );
    assert!(result.output.stderr.is_empty());
}

#[tokio::test]
async fn startup_errors_fail_inherited_execution_but_do_not_affect_clean_execution() {
    for (filename, source) in [
        ("env.nu", "do --ignore-shell-errors { 'ignored' }"),
        ("config.nu", "$env.config.footer_mode = '25'"),
        ("config.nu", "error make {msg: 'startup-runtime-error'}"),
        ("config.nu", "exit 0"),
    ] {
        let workspace = TestWorkspace::new();
        let config_home = workspace.0.join("config");
        let nushell_config = config_home.join("nushell");
        std::fs::create_dir_all(&nushell_config).expect("create config directory");
        std::fs::write(nushell_config.join(filename), source).expect("write invalid startup file");
        for startup in [NushellStartup::Inherit, NushellStartup::Clean] {
            let mut execution = plan(b"print 'script-ran'".to_vec(), &workspace);
            execution.nushell_startup = startup;
            execution
                .environment
                .insert("XDG_CONFIG_HOME".into(), config_home.display().to_string());
            let result = execute(execution, CancellationToken::new())
                .await
                .expect("run host");
            if startup == NushellStartup::Inherit {
                assert_eq!(
                    result.output.termination,
                    Termination::Exited { code: Some(70) }
                );
                assert!(!String::from_utf8_lossy(&result.output.stdout).contains("script-ran"));
                assert!(String::from_utf8_lossy(&result.output.stderr).contains(filename));
                assert!(
                    String::from_utf8_lossy(&result.output.stderr)
                        .contains("script was not executed")
                );
            } else {
                assert_eq!(
                    result.output.termination,
                    Termination::Exited { code: Some(0) }
                );
                assert_eq!(
                    String::from_utf8_lossy(&result.output.stdout),
                    "script-ran\n"
                );
                assert!(result.output.stderr.is_empty());
            }
        }
    }
}

#[tokio::test]
async fn startup_warnings_are_quiet_without_hiding_script_diagnostics() {
    let warning =
        "$env.config.history.file_format = 'plaintext'; $env.config.history.isolation = true";
    for (script, expected_code, diagnostic) in [
        ("print 'script-ran'".to_owned(), 0, None),
        (
            "$env.config = {history: {file_format: plaintext, isolation: true}}; print 'script-ran'".to_owned(),
            0,
            Some("history isolation only compatible"),
        ),
        ("let =".to_owned(), 1, Some("nu::parser::")),
        (
            "error make {msg: 'script-runtime-error'}".to_owned(),
            1,
            Some("script-runtime-error"),
        ),
        (
            "print --stderr 'script-stderr'".to_owned(),
            0,
            Some("script-stderr"),
        ),
    ] {
        let workspace = TestWorkspace::new();
        let config_home = workspace.0.join("config");
        let nushell_config = config_home.join("nushell");
        std::fs::create_dir_all(&nushell_config).expect("create config directory");
        std::fs::write(nushell_config.join("config.nu"), warning).expect("write warning config");
        let mut execution = plan(script.into_bytes(), &workspace);
        execution.nushell_startup = NushellStartup::Inherit;
        execution
            .environment
            .insert("XDG_CONFIG_HOME".into(), config_home.display().to_string());
        let result = execute(execution, CancellationToken::new())
            .await
            .expect("run host");
        let stderr = String::from_utf8_lossy(&result.output.stderr);
        assert_eq!(
            result.output.termination,
            Termination::Exited {
                code: Some(expected_code)
            },
            "{stderr}"
        );
        if let Some(diagnostic) = diagnostic {
            assert!(stderr.contains(diagnostic), "{stderr}");
            assert!(!stderr.contains("config.nu"), "{stderr}");
        } else {
            assert!(stderr.is_empty(), "{stderr}");
            assert_eq!(
                String::from_utf8_lossy(&result.output.stdout),
                "script-ran\n"
            );
        }
    }
}
