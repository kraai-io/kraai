#[cfg(windows)]
fn main() -> std::io::Result<()> {
    kraai_sandbox::windows_helper::main()
}

#[cfg(not(windows))]
fn main() -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the sandbox helper is only needed on Windows",
    ))
}
