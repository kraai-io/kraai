use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(
    description = "Read files from the filesystem",
    name = "read_files",
    example = r#"{"files":["/etc/passwd"],"max_size":4096}"#,
    example = r#"{"files":["/etc/passwd","/etc/hosts"],"encoding":"utf-8","max_size":1048576}"#
)]
struct ReadFileArgs {
    #[toon_schema(description = "File paths to read", min = 1)]
    files: Vec<String>,

    #[toon_schema(description = "Optional encoding")]
    encoding: Option<String>,

    #[toon_schema(description = "Maximum file size in bytes")]
    max_size: i64,
}

fn main() {
    println!("{}", ReadFileArgs::toon_schema());

    let example = r#"{"files":["/etc/passwd","/etc/hosts"],"encoding":"utf-8","max_size":1048576}"#;
    let args: ReadFileArgs = serde_json::from_str(example).unwrap();
    println!("Successfully parsed example:");
    println!("  files: {:?}", args.files);
    println!("  encoding: {:?}", args.encoding);
    println!("  max_size: {}", args.max_size);
}
