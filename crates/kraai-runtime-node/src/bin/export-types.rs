fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .ok_or("expected output directory")?;
    kraai_runtime_node::export_types(std::path::Path::new(&directory))
}
