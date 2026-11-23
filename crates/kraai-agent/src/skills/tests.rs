use super::*;

#[test]
fn parses_multiline_yaml_without_loading_body() {
    let metadata = parse_metadata(
        "---\r\nname: review\r\ndescription: >-\r\n  Review code\r\n  for bugs.\r\nmetadata:\r\n  author: example\r\n---\r\nSecret body".as_bytes(),
    ).unwrap();
    assert_eq!(metadata.description, "Review code for bugs.");
    for contents in [
        "name: review",
        "---\nname: review\ndescription: ok",
        "---\nname: Bad\ndescription: ok\n---",
        "---\nname: review\ndescription: ''\n---",
        "---\nname: review\n---",
    ] {
        assert!(parse_metadata(contents.as_bytes()).is_err());
    }
}

#[test]
fn discovery_keeps_sources_separate_and_skips_invalid_skills() {
    let root = std::env::temp_dir().join(format!("kraai-skills-{}", ulid::Ulid::generate()));
    let workspace = root.join("workspace");
    let user = root.join("user");
    let workspace_skills = workspace.join(".agents/skills");
    for directory in [workspace_skills.join("review"), user.join("review")] {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("SKILL.md"),
            "---\nname: review\ndescription: Review code\n---\nSECRET INSTRUCTIONS",
        )
        .unwrap();
    }
    std::fs::create_dir_all(workspace_skills.join("broken")).unwrap();
    std::fs::write(workspace_skills.join("broken/SKILL.md"), "bad").unwrap();
    let catalog = discover_roots(&workspace, Some(&user));
    assert_eq!(catalog.skills.len(), 2);
    assert_eq!(catalog.warnings.len(), 1);
    let roots = catalog.read_roots();
    assert_eq!(roots.len(), 2);
    assert!(!roots.contains(&workspace_skills.join("broken")));
    let prompt = catalog.prompt().expect("missing catalog");
    assert!(prompt.contains("workspace:review"));
    assert!(prompt.contains("user:review"));
    assert!(prompt.contains("open --raw"));
    assert!(!prompt.contains("SECRET INSTRUCTIONS"));
    assert!(
        discover_roots(&root.join("missing"), None)
            .prompt()
            .is_none()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn discovery_accepts_symlinked_skill_directories() {
    let root = std::env::temp_dir().join(format!("kraai-skills-{}", ulid::Ulid::generate()));
    let workspace = root.join("workspace");
    let user = root.join("user");
    let installed = root.join("store/hash-unslop");
    std::fs::create_dir_all(&installed).unwrap();
    std::fs::write(
        installed.join("SKILL.md"),
        "---\nname: unslop\ndescription: Edit prose\n---\nSECRET INSTRUCTIONS",
    )
    .unwrap();
    for skills in [workspace.join(".agents/skills"), user.clone()] {
        std::fs::create_dir_all(&skills).unwrap();
        std::os::unix::fs::symlink(&installed, skills.join("unslop")).unwrap();
    }
    let catalog = discover_roots(&workspace, Some(&user));
    assert!(catalog.warnings.is_empty());
    assert_eq!(catalog.skills.len(), 2);
    assert_eq!(
        catalog.read_roots(),
        vec![installed.canonicalize().unwrap()]
    );
    for skill in &catalog.skills {
        assert_eq!(
            skill.path,
            installed.canonicalize().unwrap().join("SKILL.md")
        );
    }
    let prompt = catalog.prompt().expect("missing catalog");
    assert!(prompt.contains("user:unslop"));
    assert!(prompt.contains("workspace:unslop"));
    assert!(!prompt.contains("SECRET INSTRUCTIONS"));
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn discovery_rejects_symlinks_outside_skill_root() {
    let root = std::env::temp_dir().join(format!("kraai-skills-{}", ulid::Ulid::generate()));
    let skills = root.join(".agents/skills");
    std::fs::create_dir_all(skills.join("escape")).unwrap();
    std::fs::write(
        root.join("outside"),
        "---\nname: escape\ndescription: outside\n---",
    )
    .unwrap();
    std::os::unix::fs::symlink(root.join("outside"), skills.join("escape/SKILL.md")).unwrap();
    let catalog = discover_roots(&root, None);
    assert!(catalog.skills.is_empty());
    assert_eq!(catalog.warnings.len(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn frontmatter_limit_bounds_reads_and_accepts_exact_boundary() {
    let prefix = "---\nname: review\ndescription: Review code\n#";
    let suffix = "\n---\n";
    let padding = MAX_FRONTMATTER_BYTES - prefix.len() - suffix.len();
    let header = format!("{prefix}{}{suffix}", "x".repeat(padding));
    assert!(parse_metadata(header.as_bytes()).is_ok());
    let oversized = format!("{prefix}x{}{suffix}", "x".repeat(padding));
    assert!(parse_metadata(oversized.as_bytes()).is_err());

    let huge_line = format!("---\n{}", "x".repeat(MAX_FRONTMATTER_BYTES * 4));
    let mut reader = std::io::Cursor::new(huge_line);
    let error = parse_metadata(&mut reader).expect_err("oversized header accepted");
    assert!(error.contains("exceeds"));
    assert!(reader.position() <= MAX_FRONTMATTER_BYTES as u64 + 1);
}

#[test]
fn discovery_ignores_large_body_and_reports_oversized_header() {
    use std::io::Write;

    let root = std::env::temp_dir().join(format!("kraai-skills-{}", ulid::Ulid::generate()));
    let skills = root.join(".agents/skills");
    std::fs::create_dir_all(skills.join("review")).unwrap();
    std::fs::create_dir_all(skills.join("oversized")).unwrap();
    let mut file = std::fs::File::create(skills.join("review/SKILL.md")).unwrap();
    file.write_all(b"---\nname: review\ndescription: Review code\n---\n")
        .unwrap();
    file.write_all(&[0xff]).unwrap();
    file.set_len(1024 * 1024 * 1024).unwrap();
    std::fs::write(
        skills.join("oversized/SKILL.md"),
        format!("---\n{}", "x".repeat(MAX_FRONTMATTER_BYTES + 1)),
    )
    .unwrap();
    let catalog = discover_roots(&root, None);
    assert_eq!(catalog.skills.len(), 1);
    assert_eq!(catalog.warnings.len(), 1);
    assert!(
        catalog
            .warnings
            .iter()
            .any(|warning| warning.contains("exceeds"))
    );
    std::fs::remove_dir_all(root).unwrap();
}
