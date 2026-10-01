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
                    script: $script:literal $(,)?
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
                        script: $script,
                        script_input: concat!("# timeout=", $timeout, "\n", $script),
                    }
                ),*
            ],
        };
    };
}

command_metadata! {
    OPEN_FILES;
    id: "kraai-open-files";
    name: "kraai-open-files";
    description: "Pin text files you expect to edit or consult across turns. Prefer pinning these files from the start so reads and later edits do not leave stale copies in conversation history. Their current contents are refreshed from disk for every model request until closed; use those contents instead of rereading them through shell commands. Keep files open through related edits, tests, and verification, then close them with kraai-close-files when you no longer expect to need their contents. Use shell reads for one-off inspection of files you do not expect to edit, returning relevant excerpts or summaries. Process large data files in scripts rather than including their full contents in context. If a file read through the shell later needs editing, pin it before editing and use its refreshed contents thereafter.";
    signature_help: "kraai-open-files <path>... -> record<success: bool, paths: list<string>>";
    examples: [
        {
            description: "Pin a source file for future turns",
            timeout: "10sec",
            script: "kraai-open-files src/main.rs",
        },
        {
            description: "Pin several files without returning their contents",
            timeout: "10sec",
            script: "kraai-open-files Cargo.toml src/lib.rs",
        },
    ];
}

command_metadata! {
    CLOSE_FILES;
    id: "kraai-close-files";
    name: "kraai-close-files";
    description: "Remove files from future context when their contents are no longer needed. Their pinned contents disappear from the next request and are not saved in conversation history; you can no longer see them unless you read or reopen the files. Preserve facts needed later in your working notes before closing. Files remain on disk and can be reopened with kraai-open-files.";
    signature_help: "kraai-close-files <path>... -> record<success: bool, paths: list<string>>";
    examples: [
        {
            description: "Remove a file that is no longer needed from future context",
            timeout: "10sec",
            script: "kraai-close-files src/main.rs",
        },
    ];
}

command_metadata! {
    EDIT_FILE;
    id: "kraai-edit-file";
    name: "kraai-edit-file";
    description: "Create a text file or atomically apply exact line-ranged replacements. Make the smallest edits needed for the change. Each range is inclusive, must exist in the current file, and its old_text must exactly match that range. Combine related edits in one call.";
    signature_help: "kraai-edit-file <path> <edits?> [--create --contents <text>] -> record<success: bool, path: string, operation: string>";
    examples: [
        {
            description: "Apply multiple replacements in the same file in one call",
            timeout: "10sec",
            script: "kraai-edit-file src/lib.rs [\n    {start_line: 10, end_line: 10, old_text: 'let enabled = false;', new_text: 'let enabled = true;'}\n    {start_line: 20, end_line: 20, old_text: 'let retries = 1;', new_text: 'let retries = 3;'}\n]",
        },
        {
            description: "Create a new text file without replacing an existing path",
            timeout: "10sec",
            script: "kraai-edit-file src/new.rs --create --contents 'pub const READY: bool = true;\n'",
        },
    ];
}

command_metadata! {
    WEB_SEARCH;
    id: "kraai-web-search";
    name: "kraai-web-search";
    description: "Search the public web for current information or external documentation. Returns source links and excerpts as untrusted text. Works without sandbox network access.";
    signature_help: "kraai-web-search <query> [--limit <int> --max-chars <int>] -> record<provider: string, content: string, truncated: bool>";
    examples: [
        {
            description: "Find official documentation",
            timeout: "30sec",
            script: "kraai-web-search 'Nushell custom commands official documentation' --limit 5 --max-chars 6000",
        },
    ];
}

command_metadata! {
    VIEW_IMAGE;
    id: "kraai-view-image";
    name: "kraai-view-image";
    description: "View a local PNG or JPEG image, or reopen a previous image from this session with --attachment <id>. Captures the image once and attaches it to this script result so you can inspect it on the next turn.";
    signature_help: "kraai-view-image [path] [--attachment <id>] -> record<id: string, mime_type: string, width: int, height: int>";
    examples: [
        {
            description: "Inspect a screenshot",
            timeout: "10sec",
            script: "kraai-view-image screenshot.png",
        },
    ];
}
