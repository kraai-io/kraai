use super::find_program_in;

#[test]
fn respects_path_and_executable_extension_order() {
    let root = std::env::temp_dir().join(format!("kraai-executable-{}", ulid::Ulid::generate()));
    std::fs::create_dir_all(root.join("first")).unwrap();
    std::fs::create_dir(root.join("second")).unwrap();
    std::fs::write(root.join("first/cargo.cmd"), b"").unwrap();
    std::fs::write(root.join("second/cargo.exe"), b"").unwrap();
    let path = std::env::join_paths([root.join("first"), root.join("second")])
        .map_err(std::io::Error::other)
        .unwrap();
    assert_eq!(
        find_program_in("cargo", &path, ".exe;.cmd"),
        Some(root.join("first/cargo.cmd"))
    );
    assert_eq!(
        find_program_in("cargo.exe", &path, ".exe;.cmd"),
        Some(root.join("second/cargo.exe"))
    );
    assert_eq!(find_program_in("missing", &path, ".exe;.cmd"), None);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "linux")]
mod linux;
