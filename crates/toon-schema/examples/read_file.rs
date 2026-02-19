use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Read files from the filesystem", name = "read_file")]
struct ReadFileArgs {
    #[toon_schema(
        description = "List of file paths to read",
        example = "[\"/etc/passwd\", \"/etc/hosts\"]"
    )]
    files: Vec<String>,

    #[toon_schema(description = "Optional encoding", example = "\"utf-8\"")]
    encoding: Option<String>,

    #[toon_schema(description = "Maximum file size in bytes", example = "1048576")]
    max_size: i64,
}

fn main() {
    println!("=== Tool Name ===");
    println!("{}", ReadFileArgs::tool_name());

    println!("\n=== Schema ===");
    println!("{}", ReadFileArgs::toon_schema());

    println!("\n=== Validation Test ===");
    // Verify the example can be deserialized
    let example = r#"{"files":["/etc/passwd","/etc/hosts"],"encoding":"utf-8","max_size":1048576}"#;
    let args: ReadFileArgs = serde_json::from_str(example).unwrap();
    println!("Successfully parsed example:");
    println!("  Files: {:?}", args.files);
    println!("  Encoding: {:?}", args.encoding);
    println!("  Max size: {}", args.max_size);
}
