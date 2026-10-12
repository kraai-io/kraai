#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandMetadata {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub signature_help: &'static str,
    pub examples: &'static [CommandExample],
    pub services: &'static [HostService],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandExample {
    pub description: &'static str,
    pub setup: &'static str,
    pub script: &'static str,
    pub script_input: &'static str,
    pub outcome: &'static str,
}

/// Runtime services a command may request from the parent process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostService {
    Mcp,
    WebSearch,
    Images,
    StateEffects,
}

macro_rules! command_metadata {
    (
        id: $id:literal;
        description: $description:literal;
        signature_help: $signature_help:literal;
        services: [$($service:ident),* $(,)?];
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
        $crate::CommandMetadata {
            id: $id,
            name: $id,
            description: $description,
            signature_help: $signature_help,
            services: &[$($crate::HostService::$service),*],
            examples: &[
                $(
                    $crate::CommandExample {
                        description: $example_description,
                        setup: concat!($($setup)?),
                        script: $script,
                        script_input: concat!("# timeout=", $timeout, "\n", $script),
                        outcome: concat!($($outcome)?),
                    }
                ),*
            ],
        }
    };
}
