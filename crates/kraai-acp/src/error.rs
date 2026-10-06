use agent_client_protocol::Error;
use kraai_runtime::{RuntimeError, RuntimeErrorKind};

pub(crate) fn runtime(error: RuntimeError) -> Error {
    match error.kind {
        RuntimeErrorKind::InvalidArgument
        | RuntimeErrorKind::Validation
        | RuntimeErrorKind::NotFound
        | RuntimeErrorKind::Conflict => invalid(error.message),
        _ => Error::internal_error().data(error.message),
    }
}

pub(crate) fn invalid(message: impl Into<String>) -> Error {
    Error::invalid_params().data(message.into())
}

pub(crate) fn internal(message: impl Into<String>) -> Error {
    Error::internal_error().data(message.into())
}
