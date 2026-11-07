use super::{BashRewrite, rewrite_bash_habits};
use crate::StartupFile;

fn rewrite(script: &str) -> Option<String> {
    rewrite_with_startup(script, "")
}

fn every_command() -> Vec<String> {
    kraai_commands::command_ids().map(String::from).collect()
}

/// Rewrites `script` for a shell with every Kraai command and `config` as its `config.nu`.
fn rewrite_with_startup(script: &str, config: &str) -> Option<String> {
    let startup = (!config.is_empty()).then(|| StartupFile {
        path: "/home/user/.config/nushell/config.nu".into(),
        contents: config.as_bytes().to_vec(),
    });
    rewrite_bash_habits(script, &every_command(), startup.as_slice()).map(|rewrite| rewrite.source)
}

const REWRITES: [(&str, &str); 19] = [
    (
        "grep -rn foo src 2>/dev/null",
        "grep -rn foo src e> /dev/null",
    ),
    ("ls src 2>/dev/null", "ls src e> /dev/null"),
    ("^git status 2> /dev/null", "^git status e> /dev/null"),
    ("rg foo 2>>errors.log", "rg foo e>> errors.log"),
    ("cargo build 2>&1 | tail -20", "cargo build o+e>| tail -20"),
    ("nix --version 2>&1", "nix --version o+e>| collect"),
    ("ls 2>&1 | length", "ls  | length"),
    (
        "gh run view 1 --log > /tmp/run.log 2>&1; print done",
        "gh run view 1 --log o+e> /tmp/run.log ; print done",
    ),
    ("git diff > /tmp/diff.txt", "git diff o> /tmp/diff.txt"),
    ("git diff >/tmp/diff.txt", "git diff o> /tmp/diff.txt"),
    ("git log >> /tmp/log.txt", "git log o>> /tmp/log.txt"),
    ("make &>/dev/null", "make o+e> /dev/null"),
    ("print $HOME", "print $env.HOME"),
    ("ls $HOME/.config", "ls $\"($env.HOME)/.config\""),
    (
        "let out = (^cargo --version 2>&1)",
        "let out = (^cargo --version o+e>| collect)",
    ),
    (
        "let out = cargo --version 2>&1",
        "let out = cargo --version o+e>| collect",
    ),
    (
        "[a b] | each {|dir| rg foo $dir 2>/dev/null }",
        "[a b] | each {|dir| rg foo $dir e> /dev/null }",
    ),
    (
        "if true {\n  rg foo 2>/dev/null\n}",
        "if true {\n  rg foo e> /dev/null\n}",
    ),
    (
        "# list the workspace\ngit status 2>/dev/null\nls",
        "# list the workspace\ngit status e> /dev/null\nls",
    ),
];

#[cfg(not(windows))]
const ROUTED: [(&str, &str); 6] = [
    ("find . -name '*.rs'", "^find . -name '*.rs'"),
    ("ps aux | head", "^ps aux | head"),
    ("mkdir -p a/b", "^mkdir -p a/b"),
    ("echo -e 'a'", "^echo -e 'a'"),
    ("ls -R src", "^ls -R src"),
    (
        "find . -name '*.rs' 2>/dev/null | head",
        "^find . -name '*.rs' e> /dev/null | head",
    ),
];

#[test]
fn rewrites_bash_habits_into_equivalent_nushell() {
    for (input, expected) in REWRITES {
        assert_eq!(rewrite(input).as_deref(), Some(expected), "{input}");
    }
}

#[cfg(not(windows))]
#[test]
fn routes_bash_calls_of_shadowed_builtins_to_the_external_program() {
    for (input, expected) in ROUTED {
        assert_eq!(rewrite(input).as_deref(), Some(expected), "{input}");
    }
}

#[test]
fn rewritten_scripts_need_no_further_rewrite() {
    let rewritten = REWRITES.iter().map(|(_, rewritten)| *rewritten);
    #[cfg(not(windows))]
    let rewritten = rewritten.chain(ROUTED.iter().map(|(_, rewritten)| *rewritten));
    for script in rewritten {
        assert_eq!(rewrite(script), None, "{script}");
    }
}

#[test]
fn leaves_nushell_and_ambiguous_bash_alone() {
    for input in [
        "ls | where size > 10kb",
        "if 3 > 2 { print yes }",
        "^awk '$1 > 0' data.txt",
        "rg '2>/dev/null' src",
        "print \"2>&1\"",
        "let HOME_DIR = 1; print $HOME_DIR",
        "ls -la",
        "[a b] | find a",
        "date now | format date '%Y'",
        "^echo hi >&2",
        "kraai-open-files src/lib.rs",
        // `&&` and `||` have no Nushell form with the same failure behavior.
        "git status && git diff",
        "git diff 2>/dev/null || git status",
        "find . -name x && ls",
        "print $undefined_lowercase",
    ] {
        assert_eq!(rewrite(input), None, "{input}");
    }
}

#[cfg(not(windows))]
#[test]
fn user_aliases_and_definitions_keep_their_meaning() {
    let config =
        "alias ps = ^procs\ndef --wrapped find [...rest] { ^fd ...$rest }\nalias ll = ls -l\n";
    for script in ["ps aux", "find . -name x", "ll -R"] {
        assert_eq!(rewrite_with_startup(script, config), None, "{script}");
    }
    assert_eq!(
        rewrite_with_startup("mkdir -p a/b", config).as_deref(),
        Some("^mkdir -p a/b")
    );
}

#[test]
fn startup_files_that_do_not_parse_disable_rewrites() {
    assert_eq!(rewrite_with_startup("git diff > out.txt", "let ="), None);
    assert_eq!(
        rewrite_with_startup("git diff > out.txt", "alias ll = ls -l").as_deref(),
        Some("git diff o> out.txt")
    );
}

const HEREDOCS: [(&str, &str); 8] = [
    (
        "cat > src/lib.rs <<'RS'\n#[derive(Debug)]\nstruct A;\nRS",
        "kraai-edit-file src/lib.rs --overwrite --contents r##'#[derive(Debug)]\nstruct A;\n'##",
    ),
    (
        "cat > src/lib.rs <<'RS'\nfn main() {}\nRS\ncargo build",
        "kraai-edit-file src/lib.rs --overwrite --contents r#'fn main() {}\n'#\ncargo build",
    ),
    (
        "cat <<'EOF' > notes.txt\nit's 'quoted'#\nEOF",
        "kraai-edit-file notes.txt --overwrite --contents r##'it's 'quoted'#\n'##",
    ),
    (
        "cat > a.txt <<EOF\nplain text\nEOF\n",
        "kraai-edit-file a.txt --overwrite --contents r#'plain text\n'#\n",
    ),
    (
        "python3 - <<'PY'\nprint('hi')\nPY",
        "r#'print('hi')\n'# | python3 -",
    ),
    (
        "if true {\n  cat > a.txt <<'EOF'\nx\nEOF\n}",
        "if true {\n  kraai-edit-file a.txt --overwrite --contents r#'x\n'#\n}",
    ),
    (
        "cat > a.txt <<'EOF'\nx\nEOF\ncargo test 2>&1 | tail -n 5",
        "kraai-edit-file a.txt --overwrite --contents r#'x\n'#\ncargo test o+e>| tail -n 5",
    ),
    (
        "cat > a.rs <<'A'\nfn a() {}\nA\ncat > b.rs <<'B'\nlet c = 'c';\nB",
        "kraai-edit-file a.rs --overwrite --contents r#'fn a() {}\n'#\nkraai-edit-file b.rs --overwrite --contents r#'let c = 'c';\n'#",
    ),
];

#[test]
fn heredocs_become_whole_file_writes_or_piped_text() {
    for (input, expected) in HEREDOCS {
        assert_eq!(rewrite(input).as_deref(), Some(expected), "{input}");
    }
}

#[test]
fn heredocs_save_files_when_the_shell_lacks_the_edit_command() {
    let commands: Vec<String> = every_command()
        .into_iter()
        .filter(|command| command != kraai_commands::edit_file::METADATA.id)
        .collect();
    let rewrite = rewrite_bash_habits("cat > a.txt <<'EOF'\nx\nEOF", &commands, &[]);
    assert_eq!(
        rewrite.map(|rewrite| rewrite.source).as_deref(),
        Some("r#'x\n'# | save -f a.txt")
    );
}

#[test]
fn heredocs_without_a_safe_rewrite_leave_the_script_alone() {
    for input in [
        "cat > a.txt <<EOF\n$HOME\nEOF\nls 2>/dev/null",
        "cat >> a.txt <<'EOF'\nx\nEOF",
        "git diff | cat > a.txt <<'EOF'\nx\nEOF",
        "cat > a.txt <<'EOF'\nno terminator",
        "print r#'\ncat > a.txt <<'EOF'\nx\nEOF\n'#",
    ] {
        assert_eq!(rewrite(input), None, "{input}");
    }
}

#[test]
fn rewrites_report_each_change_once() {
    let rewrite = |script: &str| {
        rewrite_bash_habits(script, &every_command(), &[]).map(|rewrite| rewrite.changes)
    };
    assert_eq!(
        rewrite("rg a 2>/dev/null; rg b 2>/dev/null"),
        Some(vec![String::from("`2>` → `e>`")])
    );
    assert_eq!(
        rewrite("cat > a.txt <<'EOF'\nx\nEOF\ncargo test 2>&1 | tail -n 5; print $HOME"),
        Some(vec![
            String::from(
                "`cat > a.txt <<'EOF'` → `kraai-edit-file a.txt --overwrite --contents r#'…'#`"
            ),
            String::from("`2>&1 |` → `o+e>|`"),
            String::from("`$HOME` → `$env.HOME`"),
        ])
    );
    assert_eq!(
        rewrite("ls 2>&1 | length"),
        Some(vec![String::from("removed `2>&1`")])
    );
}

#[cfg(not(windows))]
#[test]
fn routed_programs_report_the_caret() {
    let rewrite = rewrite_bash_habits("find . -name x", &every_command(), &[]);
    assert_eq!(
        rewrite,
        Some(BashRewrite {
            source: String::from("^find . -name x"),
            changes: vec![String::from("`find` → `^find`")],
        })
    );
}
