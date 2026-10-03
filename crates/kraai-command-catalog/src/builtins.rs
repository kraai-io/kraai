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
    OPEN_FILES;
    id: "kraai-open-files";
    name: "kraai-open-files";
    description: "Read text files into context. Their contents stay available and refresh automatically before each response, replacing any earlier snapshot. Use the supplied contents without rereading the files. Close files with kraai-close-files when their contents are no longer needed. For large data files, use scripts to return relevant excerpts or summaries instead.";
    signature_help: "kraai-open-files <path>... -> record<success: bool, paths: list<string>>";
    examples: [
        {
            description: "Read a source file into context",
            timeout: "10sec",
            script: "kraai-open-files src/main.rs",
            outcome: "Returns success and the resolved paths. A line-numbered snapshot is supplied alongside the script result and stays current until closed.",
        },
        {
            description: "Read several files into context",
            timeout: "10sec",
            script: "kraai-open-files Cargo.toml src/lib.rs",
        },
    ];
}

command_metadata! {
    CLOSE_FILES;
    id: "kraai-close-files";
    name: "kraai-close-files";
    description: "Remove file snapshots from subsequent model requests. Their contents are no longer visible unless you reopen or read the files. Files remain on disk.";
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
    description: "Create a text file or atomically replace complete line ranges. Line numbers are 1-based and inclusive; every edit in a call refers to the original file. Ranges must exist and cannot overlap. old_text must match all bytes in the range, including its final newline if present, but excluding displayed line-number prefixes. new_text is inserted verbatim: include every newline you want, using \\n for LF or \\r\\n for CRLF in Nushell double-quoted strings. No newline is added automatically. An empty new_text deletes the complete range. Combine related edits in one call.";
    signature_help: "kraai-edit-file <path> <edits?> [--create --contents <text>] -> record<success: bool, path: string, operation: string>";
    examples: [
        {
            description: "Expand the first line and delete original line 3 in one call",
            timeout: "10sec",
            script: "kraai-edit-file settings.conf [\n    {start_line: 1, end_line: 1, old_text: \"enabled = false\\n\", new_text: \"enabled = true\\nverbose = false\\n\"}\n    {start_line: 3, end_line: 3, old_text: \"obsolete = true\\n\", new_text: ''}\n]",
            setup: "Before settings.conf, with LF after every line:\n```text\nenabled = false\nretries = 1\nobsolete = true\n```",
            outcome: "After settings.conf, with LF after every line:\n```text\nenabled = true\nverbose = false\nretries = 1\n```\nThe result contains success: true, the resolved path, and operation: edited.",
        },
        {
            description: "Preserve CRLF when replacing a line",
            timeout: "10sec",
            script: "kraai-edit-file windows.conf [{start_line: 1, end_line: 1, old_text: \"count = 1\\r\\n\", new_text: \"count = 2\\r\\n\"}]",
            setup: "Before windows.conf, shown as an escaped string: `\"count = 1\\r\\n\"`.",
            outcome: "After windows.conf, shown as an escaped string: `\"count = 2\\r\\n\"`.",
        },
        {
            description: "Create a new text file without replacing an existing path",
            timeout: "10sec",
            script: "kraai-edit-file notes.txt --create --contents \"ready\\n\"",
            setup: "notes.txt does not exist.",
            outcome: "notes.txt contains `ready` followed by LF. The result contains success: true, the resolved path, and operation: created.",
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
            outcome: "The content field contains source links and excerpts. The truncated field indicates whether the response reached the requested character limit.",
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
            outcome: "Returns the attachment id, MIME type, width, and height. The image is attached to this result for inspection on the next turn.",
        },
    ];
}
