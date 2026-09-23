#![forbid(unsafe_code)]

use kraai_command_core::{command_error, declare_kraai_command};
use kraai_types::WebSearchRequest;
use nu_engine::CallExt;
use nu_protocol::{Category, IntoPipelineData, Signature, SyntaxShape, Type, Value, record};

declare_kraai_command! {
    pub struct WebSearchCommand;
    metadata: kraai_command_catalog::WEB_SEARCH;
    signature: Signature::build(Self::METADATA.name)
        .required("query", SyntaxShape::String, "Search query")
        .named("limit", SyntaxShape::Int, "Result count, 1 to 10 (default 5)", None)
        .named("max-chars", SyntaxShape::Int, "Output character limit, 1 to 20000 (default 6000)", None)
        .input_output_types(vec![(Type::Nothing, Type::Record(Default::default()))])
        .category(Category::Network);
    run: |context, engine_state, stack, call, _input| {
        let query: String = call.req(engine_state, stack, 0)?;
        let limit: i64 = call.get_flag(engine_state, stack, "limit")?.unwrap_or(5);
        let max_chars: i64 = call.get_flag(engine_state, stack, "max-chars")?.unwrap_or(6000);
        let invalid = |message| command_error("Invalid web search", message, call.head);
        let request = WebSearchRequest {
            query,
            limit: usize::try_from(limit).map_err(|_error| invalid("limit must be positive"))?,
            max_chars: usize::try_from(max_chars).map_err(|_error| invalid("max-chars must be positive"))?,
        };
        request.validate().map_err(|error| command_error("Invalid web search", error, call.head))?;
        let response = context.web_search().search(request)
            .map_err(|error| command_error("Web search failed", error, call.head))?;
        Ok(Value::record(record! {
            "provider" => Value::string(response.provider, call.head),
            "content" => Value::string(response.content, call.head),
            "truncated" => Value::bool(response.truncated, call.head),
        }, call.head).into_pipeline_data())
    }
}
