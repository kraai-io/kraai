fn main() -> Result<(), embed_resource::CompilationResult> {
    println!("cargo:rerun-if-changed=../../assets/brand/kraai.rc");
    println!("cargo:rerun-if-changed=../../assets/brand/kraai.ico");
    embed_resource::compile("../../assets/brand/kraai.rc", embed_resource::NONE).manifest_required()
}
