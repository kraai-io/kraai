use super::*;

#[test]
fn execution_ids_reject_path_syntax() {
    for invalid in [
        "../sessions",
        "/tmp/message",
        r"..\sessions",
        r"C:\temp\message",
        r"\\server\share",
        ".",
    ] {
        assert!(
            ScriptExecutionId::try_new(invalid).is_err(),
            "accepted {invalid:?}"
        );
        assert!(
            CommandInvocationId::try_new(invalid).is_err(),
            "accepted {invalid:?}"
        );
    }
    assert!(ScriptExecutionId::try_new("01J_VALID-execution_id").is_ok());
    assert!(CommandInvocationId::try_new("01J_VALID-invocation_id").is_ok());
}
