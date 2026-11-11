#![forbid(unsafe_code)]

mod duration;
mod error;
mod payload;
mod result;
mod result_reader;

pub use error::ProtocolError;
pub use payload::{
    DEFAULT_SCRIPT_TIMEOUT, InvalidScriptBlock, ScriptBlock, parse_script_input,
    split_script_header,
};
pub use result::{
    CHANNEL_DISPLAY_LIMIT, ToolCallResultView, channel_needs_output_file, render_tool_call_result,
};
pub use result_reader::{ParsedToolCallResult, ToolCallResultSection, parse_tool_call_result};
