use super::*;

#[test]
fn nushell_host_uses_frontend_when_enabled_and_external_host_otherwise() {
    let directory =
        std::env::temp_dir().join(format!("kraai-host-resolution-{}", ulid::Ulid::generate()));
    std::fs::create_dir_all(&directory).unwrap();
    let frontend = directory.join("kraai");
    std::fs::write(&frontend, []).unwrap();
    let frontend = frontend.canonicalize().unwrap();

    let embedded = resolve_nushell_host_from(frontend.clone(), true).unwrap();
    if embedded.executable != frontend {
        panic!("frontend host selected the wrong executable");
    }
    if embedded.arguments
        != [std::ffi::OsString::from(
            kraai_nushell_runtime::INTERNAL_HOST_ARGUMENT,
        )]
    {
        panic!("frontend host omitted the internal host argument");
    }
    if resolve_nushell_host_from(frontend.clone(), false).is_ok() {
        panic!("frontend host was enabled without opt-in");
    }

    let packaged = directory.join(format!(
        "kraai-nushell-host{}",
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::write(&packaged, []).unwrap();
    let packaged = packaged.canonicalize().unwrap();
    let resolved = resolve_nushell_host_from(frontend.clone(), true).unwrap();
    if resolved.executable != embedded.executable || resolved.arguments != embedded.arguments {
        panic!("packaged host overrode the frontend host");
    }
    let resolved = resolve_nushell_host_from(frontend, false).unwrap();
    if resolved.executable != packaged || !resolved.arguments.is_empty() {
        panic!("packaged host was not selected");
    }

    let explicit = directory.join("custom-host");
    std::fs::write(&explicit, []).unwrap();
    let resolved = resolve_nushell_host(Some(&explicit), false).unwrap();
    if resolved.executable != explicit.canonicalize().unwrap() || !resolved.arguments.is_empty() {
        panic!("explicit host was not selected");
    }
    if resolve_nushell_host(Some(&directory.join("missing-host")), true).is_ok() {
        panic!("invalid explicit host silently fell back");
    }

    std::fs::remove_dir_all(directory).unwrap();
}
