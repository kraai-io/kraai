#![forbid(unsafe_code)]

mod result;
mod values;

use kraai_command_core::{command_error, declare_kraai_command};
use kraai_types::McpRequest;
use nu_engine::CallExt;
use nu_protocol::{Category, IntoPipelineData, Signature, SyntaxShape, Type, Value};

declare_kraai_command! {
    pub struct McpCommand;
    metadata: kraai_command_catalog::MCP;
    signature: Signature::build(Self::METADATA.name)
        .required("operation", SyntaxShape::String, "servers, tools, describe, search, or call")
        .optional("server-or-query", SyntaxShape::String, "Configured server alias, or search query")
        .optional("tool", SyntaxShape::String, "Exact MCP tool name")
        .optional("arguments", SyntaxShape::Record(Default::default()), "Tool arguments as a record")
        .named("limit", SyntaxShape::Int, "Maximum search results, 1 to 20 (default 5)", None)
        .input_output_types(vec![(Type::Nothing, Type::Any)])
        .category(Category::Network);
    run: |context, engine_state, stack, call, _input| {
        let operation: String = call.req(engine_state, stack, 0)?;
        let request = match operation.as_str() {
            "servers" => McpRequest::Servers,
            "tools" => McpRequest::Tools { server: call.req(engine_state, stack, 1)? },
            "describe" => McpRequest::Describe { server: call.req(engine_state, stack, 1)?, tool: call.req(engine_state, stack, 2)? },
            "search" => {
                let limit: i64 = call.get_flag(engine_state, stack, "limit")?.unwrap_or(5);
                McpRequest::Search {
                    query: call.req(engine_state, stack, 1)?,
                    limit: usize::try_from(limit).map_err(|_error| command_error("Invalid MCP search", "limit must be positive", call.head))?,
                }
            }
            "call" => {
                let arguments: Value = call.req(engine_state, stack, 3)?;
                let serde_json::Value::Object(arguments) = values::to_json(arguments).map_err(|error| *error)? else {
                    return Err(command_error("Invalid MCP arguments", "Expected a record", call.head));
                };
                McpRequest::Call { server: call.req(engine_state, stack, 1)?, tool: call.req(engine_state, stack, 2)?, arguments }
            }
            _ => return Err(command_error("Invalid MCP operation", "Expected servers, tools, describe, search, or call", call.head)),
        };
        request.validate().map_err(|error| command_error("Invalid MCP request", error, call.head))?;
        let is_call = matches!(request, McpRequest::Call { .. });
        let response = context.mcp().execute(request).map_err(|error| command_error("MCP request failed", error, call.head))?;
        let response = if is_call { result::prepare(response, context, call.head).map_err(|error| *error)? } else { response };
        Ok(values::from_json(response, call.head).map_err(|error| *error)?.into_pipeline_data())
    }
}

fn boxed_error(
    title: impl Into<String>,
    message: impl Into<String>,
    span: nu_protocol::Span,
) -> Box<nu_protocol::ShellError> {
    Box::new(command_error(title, message, span))
}
