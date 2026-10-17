use agent_client_protocol::Error;
use kraai_runtime::{RuntimeError, RuntimeErrorKind};

pub(crate) fn runtime(error: RuntimeError) -> Error {
    match error.kind {
        RuntimeErrorKind::InvalidArgument
        | RuntimeErrorKind::Validation
        | RuntimeErrorKind::NotFound
        | RuntimeErrorKind::Conflict => invalid(error.message),
        _ => internal(error.message),
    }
}

/// Clients show `message`, so it carries the reason instead of the generic code name.
fn with_message(mut error: Error, message: impl Into<String>) -> Error {
    error.message = message.into();
    error
}

pub(crate) fn invalid(message: impl Into<String>) -> Error {
    with_message(Error::invalid_params(), message)
}

pub(crate) fn internal(message: impl Into<String>) -> Error {
    with_message(Error::internal_error(), message)
}
