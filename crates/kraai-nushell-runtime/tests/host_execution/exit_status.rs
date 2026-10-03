use super::output::assert_script;

#[tokio::test]
async fn successful_externals_set_exit_status_before_the_next_statement() {
    for setup in ["hide-env -i LAST_EXIT_CODE", "$env.LAST_EXIT_CODE = 7"] {
        for (command, output) in [
            ("^/bin/sh -c 'exit 0'", ""),
            ("^/bin/sh -c 'printf hello'", "hello"),
        ] {
            assert_script(
                &format!("{setup}; {command}; $env.LAST_EXIT_CODE"),
                &format!("{output}0\n"),
            )
            .await;
        }
    }
}

#[tokio::test]
async fn rendering_values_does_not_overwrite_exit_status() {
    assert_script(
        r#"
$env.LAST_EXIT_CODE = 7
"value"
{a: 1}
[1 2] | each { $in }
print "printed"
let silent = "assigned"
"ignored" | ignore
$env.LAST_EXIT_CODE
"#,
        "value\n{\"a\":1}\n1\n2\nprinted\n7\n",
    )
    .await;
}

#[tokio::test]
async fn redirected_externals_preserve_nushell_exit_status_behavior() {
    assert_script(
        r#"
$env.LAST_EXIT_CODE = 7
^/bin/sh -c 'printf hello' out> result.txt
$env.LAST_EXIT_CODE
open --raw result.txt
"#,
        "7\nhello",
    )
    .await;
}

#[tokio::test]
async fn complete_preserves_captured_failure_and_later_success_updates_exit_status() {
    assert_script(
        r#"
$env.LAST_EXIT_CODE = 9
let result = (^/bin/sh -c 'exit 7' | complete)
$result.exit_code
$env.LAST_EXIT_CODE
^/bin/sh -c 'exit 0'
$env.LAST_EXIT_CODE
"#,
        "7\n9\n0\n",
    )
    .await;
}

#[tokio::test]
async fn caught_external_failure_then_success_refreshes_exit_status() {
    assert_script(
        r#"
try { ^/bin/sh -c 'exit 7' } catch { null }
$env.LAST_EXIT_CODE
^/bin/sh -c 'exit 0'
$env.LAST_EXIT_CODE
"#,
        "7\n0\n",
    )
    .await;
}
