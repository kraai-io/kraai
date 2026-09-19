#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandMetadata {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub signature_help: &'static str,
    pub examples: &'static [CommandExample],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandExample {
    pub description: &'static str,
    pub script: &'static str,
    pub script_input: &'static str,
}
