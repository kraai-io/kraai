use kraai_toon_schema::toon_tool;

toon_tool! {
    name: "read_files",
    description: "Read files from the filesystem",
    types: {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct ReadFileArgs {
            #[toon_schema(description = "File paths to read", min = 1)]
            files: Vec<String>,
            #[toon_schema(description = "Optional encoding")]
            encoding: Option<String>,
            #[toon_schema(description = "Maximum file size in bytes")]
            max_size: i64,
        }
    },
    root: ReadFileArgs,
    examples: [
        {
            files: ["/etc/passwd", "/etc/hosts"],
            encoding: "utf-8",
            max_size: 1048576
        }
    ]
}

fn main() {
    println!("{}", ReadFileArgs::toon_schema());
}
