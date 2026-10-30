#![forbid(unsafe_code)]

mod duration;
mod error;
mod payload;
mod result;

pub use error::ProtocolError;
pub use payload::{DEFAULT_SCRIPT_TIMEOUT, InvalidScriptBlock, ScriptBlock, parse_script_input};
pub use result::{
    CHANNEL_DISPLAY_LIMIT, ToolCallResultView, channel_needs_output_file, render_tool_call_result,
};
