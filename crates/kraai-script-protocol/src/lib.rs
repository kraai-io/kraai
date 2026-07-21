#![forbid(unsafe_code)]

mod duration;
mod error;
mod parser;
mod result;
mod start_tag;

pub use error::ProtocolError;
pub use parser::{IngestResult, InvalidScriptBlock, ScriptBlock, ScriptProtocolParser};
pub use result::{ToolCallResultView, render_tool_call_result};
