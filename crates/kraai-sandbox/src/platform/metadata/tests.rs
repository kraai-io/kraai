use std::path::PathBuf;

use crate::temp_dir::PrivateTempDir;

use super::protected_paths;

fn fixture() -> (PrivateTempDir, PathBuf) {
    let temp = PrivateTempDir::create(None).expect("create temporary directory");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).expect("create workspace");
    let workspace = workspace.canonicalize().expect("resolve workspace");
    (temp, workspace)
}

#[test]
fn git_pointer_and_common_directory_are_protected() {
    for absolute in [false, true] {
        let (_temp, workspace) = fixture();
        let git_dir = workspace.join("gitdir");
        let common_dir = workspace.join("common");
        std::fs::create_dir(&git_dir).expect("create Git directory");
        std::fs::create_dir(&common_dir).expect("create Git common directory");
        let target = if absolute {
            git_dir.clone()
        } else {
            PathBuf::from("gitdir")
        };
        std::fs::write(
            workspace.join(".git"),
            format!("gitdir: {}\r\n", target.display()),
        )
        .expect("write Git pointer");
        std::fs::write(git_dir.join("commondir"), "../common\n").expect("write common pointer");

        let protected = protected_paths(&workspace).expect("discover metadata");

        for expected in [workspace.join(".git"), git_dir, common_dir] {
            assert!(
                protected.contains(&expected),
                "unprotected: {}",
                expected.display()
            );
        }
    }
}

#[test]
fn symlinked_git_pointer_resolves_relative_to_the_workspace() {
    let (temp, workspace) = fixture();
    let git_dir = workspace.join("gitdir");
    std::fs::create_dir(&git_dir).expect("create Git directory");
    let pointer = temp.path().join("pointer");
    std::fs::write(&pointer, "gitdir: gitdir\n").expect("write Git pointer");
    std::os::unix::fs::symlink(&pointer, workspace.join(".git")).expect("link Git pointer");

    let protected = protected_paths(&workspace).expect("discover metadata");

    assert!(protected.contains(&workspace.join(".git")));
    assert!(protected.contains(&pointer.canonicalize().expect("resolve pointer")));
    assert!(protected.contains(&git_dir));
}

#[test]
fn metadata_pointer_preserves_spaces_in_the_target() {
    let (_temp, workspace) = fixture();
    let git_dir = workspace.join("gitdir ");
    std::fs::create_dir(&git_dir).expect("create Git directory");
    std::fs::write(workspace.join(".git"), "gitdir: gitdir \n").expect("write Git pointer");

    assert!(
        protected_paths(&workspace)
            .expect("discover metadata")
            .contains(&git_dir)
    );
}

#[test]
fn symlinked_common_directory_pointer_is_also_protected() {
    let (_temp, workspace) = fixture();
    let git_dir = workspace.join(".git");
    let common_dir = workspace.join("common");
    let pointer = workspace.join("common-pointer");
    std::fs::create_dir(&git_dir).expect("create Git directory");
    std::fs::create_dir(&common_dir).expect("create common directory");
    std::fs::write(&pointer, "../common\n").expect("write common pointer");
    std::os::unix::fs::symlink(&pointer, git_dir.join("commondir")).expect("link common pointer");

    let protected = protected_paths(&workspace).expect("discover metadata");

    assert!(protected.contains(&pointer));
    assert!(protected.contains(&common_dir));
}

#[test]
fn dangling_common_directory_pointer_fails_closed() {
    let (_temp, workspace) = fixture();
    std::fs::create_dir(workspace.join(".git")).expect("create Git directory");
    std::os::unix::fs::symlink("../missing", workspace.join(".git/commondir"))
        .expect("link missing pointer");
    assert!(protected_paths(&workspace).is_err());
}

#[test]
fn mutable_git_directory_aliases_fail_closed() {
    for nested in [false, true] {
        let (_temp, workspace) = fixture();
        let git_dir = workspace.join("actual/gitdir");
        std::fs::create_dir_all(&git_dir).expect("create Git directory");
        let link_target = if nested {
            workspace.join("actual")
        } else {
            git_dir
        };
        std::os::unix::fs::symlink(&link_target, workspace.join("link"))
            .expect("create metadata alias");
        let target = if nested { "link/gitdir" } else { "link" };
        std::fs::write(workspace.join(".git"), format!("gitdir: {target}\n"))
            .expect("write Git pointer");
        assert!(protected_paths(&workspace).is_err());
    }
}

#[test]
fn intermediate_metadata_aliases_fail_closed() {
    let (_temp, workspace) = fixture();
    std::fs::create_dir(workspace.join("actual")).expect("create metadata directory");
    std::os::unix::fs::symlink("actual", workspace.join("alias")).expect("create indirect alias");
    std::os::unix::fs::symlink("alias", workspace.join(".agents")).expect("create metadata alias");
    assert!(protected_paths(&workspace).is_err());
}

#[test]
fn ordinary_git_metadata_file_is_protected_without_parsing_a_target() {
    let (_temp, workspace) = fixture();
    std::fs::write(workspace.join(".git"), "ordinary content").expect("write metadata");

    let protected = protected_paths(&workspace).expect("discover metadata");

    assert!(protected.contains(&workspace.join(".git")));
    assert_eq!(protected.len(), super::PROTECTED_METADATA_NAMES.len());
}

#[test]
fn invalid_git_metadata_pointers_fail_closed() {
    for contents in [
        "gitdir: missing\n",
        "gitdir: \n",
        "gitdir: target\0ignored\n",
    ] {
        let (_temp, workspace) = fixture();
        std::fs::write(workspace.join(".git"), contents).expect("write invalid Git pointer");
        assert!(
            protected_paths(&workspace).is_err(),
            "accepted: {contents:?}"
        );
    }
    let (_temp, workspace) = fixture();
    std::fs::create_dir_all(workspace.join(".git/commondir"))
        .expect("create invalid common pointer");
    assert!(protected_paths(&workspace).is_err());
}

#[test]
fn oversized_git_metadata_pointer_is_rejected() {
    let (_temp, workspace) = fixture();
    std::fs::write(workspace.join(".git"), vec![b'x'; 64 * 1024 + 1])
        .expect("write oversized Git pointer");
    assert!(protected_paths(&workspace).is_err());
}

#[test]
fn invalid_external_pointer_contents_are_not_exposed_in_errors() {
    let (temp, workspace) = fixture();
    let external = temp.path().join("external");
    std::fs::create_dir(&external).expect("create external directory");
    std::fs::write(
        workspace.join(".git"),
        format!("gitdir: {}\n", external.display()),
    )
    .expect("write Git pointer");
    let secret = "private-pointer-contents";
    std::fs::write(external.join("commondir"), secret).expect("write external pointer");

    let error = protected_paths(&workspace).expect_err("invalid metadata must fail closed");

    assert!(!error.to_string().contains(secret));
}
