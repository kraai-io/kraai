#![forbid(unsafe_code)]

mod duration;
mod error;
mod parser;
mod payload;
mod result;

pub use error::ProtocolError;
pub use parser::{IngestResult, InvalidScriptBlock, ScriptBlock, ScriptProtocolParser};
pub use payload::{SCRIPT_METADATA_PREFIX, parse_script_input};
pub use result::{ToolCallResultView, render_tool_call_result};
