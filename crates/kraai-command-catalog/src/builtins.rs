use kraai_types::{CommandExample, CommandMetadata};

macro_rules! command_metadata {
    (
        $constant:ident;
        id: $id:literal;
        name: $name:literal;
        description: $description:literal;
        signature_help: $signature_help:literal;
        examples: [
            $(
                {
                    description: $example_description:literal,
                    timeout: $timeout:literal,
                    script: $script:literal
                    $(, setup: $setup:literal)?
                    $(, outcome: $outcome:literal)? $(,)?
                }
            ),* $(,)?
        ];
    ) => {
        pub const $constant: CommandMetadata = CommandMetadata {
            id: $id,
            name: $name,
            description: $description,
            signature_help: $signature_help,
            examples: &[
                $(
                    CommandExample {
                        description: $example_description,
                        setup: concat!($($setup)?),
                        script: $script,
                        script_input: concat!("# timeout=", $timeout, "\n", $script),
                        outcome: concat!($($outcome)?),
                    }
                ),*
            ],
        };
    };
}

command_metadata! {
    MCP;
    id: "kraai-mcp";
    name: "kraai-mcp";
    description: "Discover and call tools on configured MCP servers. Works without sandbox network access. Search returns matching definitions with schemas; describe returns one full definition. Calls return MCP content and optional structuredContent. Tool errors stop the pipeline. Images attach to the script result. Calls are never automatically retried; after a timeout or disconnect the operation may have completed.";
    signature_help: "kraai-mcp servers | tools <server> | describe <server> <tool> | search <query> [--limit <int>] | call <server> <tool> <arguments-record>";
    examples: [
        {
            description: "Find tools with their argument schemas",
            timeout: "30sec",
            script: "kraai-mcp search 'search issues' --limit 3",
        },
    ];
}

command_metadata! {
    OPEN_FILES;
    id: "kraai-open-files";
    name: "kraai-open-files";
    description: "Read text files into context as line-numbered snapshots. Contents stay available and refresh automatically when changed; no reread needed.";
    signature_help: "kraai-open-files <path>... -> record<success: bool, paths: list<string>>";
    examples: [];
}

command_metadata! {
    CLOSE_FILES;
    id: "kraai-close-files";
    name: "kraai-close-files";
    description: "Remove snapshots from context, not disk. Closed contents are no longer visible; reopen to access them.";
    signature_help: "kraai-close-files <path>... -> record<success: bool, paths: list<string>>";
    examples: [];
}

command_metadata! {
    EDIT_FILE;
    id: "kraai-edit-file";
    name: "kraai-edit-file";
    description: "Atomically replace complete line ranges using an array of edits. Ranges are 1-based, inclusive, nonoverlapping, and refer to the original file. old_text must match exactly, including line endings but excluding line-number prefixes. new_text is verbatim: supply every newline (\\n or \\r\\n in double-quoted strings); empty text deletes the range. --create requires a new path.";
    signature_help: "kraai-edit-file <path> <edits?> [--create --contents <text>] -> record<success: bool, path: string, operation: string>";
    examples: [
        {
            description: "Batch replacements and deletions",
            timeout: "10sec",
            script: "kraai-edit-file settings.conf [\n    {start_line: 1, end_line: 1, old_text: \"enabled = false\\n\", new_text: r###'enabled = true\n'###}\n    {start_line: 3, end_line: 3, old_text: \"obsolete = true\\n\", new_text: ''}\n]",
            setup: "settings.conf: \"enabled = false\\nretries = 1\\nobsolete = true\\n\"",
            outcome: "settings.conf: \"enabled = true\\nretries = 1\\n\"",
        },
    ];
}

command_metadata! {
    WEB_SEARCH;
    id: "kraai-web-search";
    name: "kraai-web-search";
    description: "Search the web; returns source links and excerpts. Works without sandbox network access.";
    signature_help: "kraai-web-search <query> [--limit <int> --max-chars <int>] -> record<provider: string, content: string, truncated: bool>";
    examples: [];
}

command_metadata! {
    VIEW_IMAGE;
    id: "kraai-view-image";
    name: "kraai-view-image";
    description: "Attach a local PNG/JPEG for inspection next turn, or reopen a session image with --attachment <id>.";
    signature_help: "kraai-view-image [path] [--attachment <id>] -> record<id: string, mime_type: string, width: int, height: int>";
    examples: [];
}
