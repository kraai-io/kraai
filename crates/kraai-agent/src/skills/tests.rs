#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert behavior and propagate fixture errors"
)]

use super::*;

#[test]
fn parses_multiline_yaml_without_loading_body() -> Result<(), String> {
    let metadata = parse_metadata(
        "---\r\nname: review\r\ndescription: >-\r\n  Review code\r\n  for bugs.\r\nmetadata:\r\n  author: example\r\n---\r\nSecret body",
    )?;
    assert_eq!(metadata.description, "Review code for bugs.");
    for contents in [
        "name: review",
        "---\nname: review\ndescription: ok",
        "---\nname: Bad\ndescription: ok\n---",
        "---\nname: review\ndescription: ''\n---",
        "---\nname: review\n---",
    ] {
        assert!(parse_metadata(contents).is_err());
    }
    Ok(())
}

#[test]
fn discovery_keeps_sources_separate_and_skips_invalid_skills()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("kraai-skills-{}", ulid::Ulid::generate()));
    let workspace = root.join("workspace");
    let user = root.join("user");
    let workspace_skills = workspace.join(".agents/skills");
    for directory in [workspace_skills.join("review"), user.join("review")] {
        std::fs::create_dir_all(&directory)?;
        std::fs::write(
            directory.join("SKILL.md"),
            "---\nname: review\ndescription: Review code\n---\nSECRET INSTRUCTIONS",
        )?;
    }
    std::fs::create_dir_all(workspace_skills.join("broken"))?;
    std::fs::write(workspace_skills.join("broken/SKILL.md"), "bad")?;
    let catalog = discover_roots(&workspace, Some(&user));
    assert_eq!(catalog.skills.len(), 2);
    assert_eq!(catalog.warnings.len(), 1);
    let prompt = catalog.prompt().ok_or("missing catalog")?;
    assert!(prompt.contains("workspace:review"));
    assert!(prompt.contains("user:review"));
    assert!(prompt.contains("open --raw"));
    assert!(!prompt.contains("SECRET INSTRUCTIONS"));
    assert!(
        discover_roots(&root.join("missing"), None)
            .prompt()
            .is_none()
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn discovery_rejects_symlinks_outside_skill_root() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("kraai-skills-{}", ulid::Ulid::generate()));
    let skills = root.join(".agents/skills");
    std::fs::create_dir_all(skills.join("escape"))?;
    std::fs::write(
        root.join("outside"),
        "---\nname: escape\ndescription: outside\n---",
    )?;
    std::os::unix::fs::symlink(root.join("outside"), skills.join("escape/SKILL.md"))?;
    let catalog = discover_roots(&root, None);
    assert!(catalog.skills.is_empty());
    assert_eq!(catalog.warnings.len(), 1);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
