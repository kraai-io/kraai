use super::*;

#[test]
fn unresolved_startup_paths_fail_inherited_startup_but_allow_clean_startup() {
    for environment in [
        nu_config::TestEnv::new(Default::default()),
        nu_config::TestEnv::new(std::collections::HashMap::from([(
            "XDG_CONFIG_HOME".to_owned(),
            "relative".to_owned(),
        )]))
        .with_home_dir(std::env::temp_dir()),
    ] {
        let mut engine = EngineState::new();
        let error = configure_startup_paths(&mut engine, NushellStartup::Inherit, &environment);
        assert!(matches!(
            error,
            Err(HostError::Initialization(message))
                if message.contains("unable to resolve startup paths")
        ));
        assert!(configure_startup_paths(&mut engine, NushellStartup::Clean, &environment).is_ok());
        assert!(!engine.config_dirs.is_resolved());
    }
}
