#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use kraai_types::ContextStateDelta;
use nu_protocol::engine::Command;
use nu_protocol::shell_error::generic::GenericError;
use nu_protocol::{ShellError, Span};

pub use kraai_types::CommandMetadata;

#[derive(Clone)]
pub struct CommandRegistration {
    id: &'static str,
    command: Box<dyn Command>,
}

impl CommandRegistration {
    pub fn new(
        id: &'static str,
        command: impl Command + Clone + 'static,
    ) -> Result<Self, CommandRegistryError> {
        if id.is_empty()
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(CommandRegistryError::InvalidId(String::from(id)));
        }
        Ok(Self {
            id,
            command: Box::new(command),
        })
    }

    pub fn id(&self) -> &'static str {
        self.id
    }

    pub fn command(&self) -> Box<dyn Command> {
        self.command.clone()
    }
}

impl std::fmt::Debug for CommandRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandRegistration")
            .field("id", &self.id)
            .field("command_name", &self.command.name())
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct CommandRegistry {
    registrations: BTreeMap<&'static str, CommandRegistration>,
}

impl CommandRegistry {
    pub fn new(
        registrations: impl IntoIterator<Item = CommandRegistration>,
    ) -> Result<Self, CommandRegistryError> {
        let mut by_id = BTreeMap::new();
        let mut names = BTreeSet::new();
        for registration in registrations {
            if by_id.contains_key(registration.id()) {
                return Err(CommandRegistryError::DuplicateId(String::from(
                    registration.id(),
                )));
            }
            let name = registration.command.name().to_owned();
            if !names.insert(name.clone()) {
                return Err(CommandRegistryError::DuplicateName(name));
            }
            by_id.insert(registration.id(), registration);
        }
        Ok(Self {
            registrations: by_id,
        })
    }

    pub fn select(
        &self,
        active_ids: &[String],
    ) -> Result<Vec<Box<dyn Command>>, CommandRegistryError> {
        let mut selected = Vec::with_capacity(active_ids.len());
        let mut seen = BTreeSet::new();
        for id in active_ids {
            if !seen.insert(id) {
                return Err(CommandRegistryError::DuplicateSelection(id.clone()));
            }
            let registration = self
                .registrations
                .get(id.as_str())
                .ok_or_else(|| CommandRegistryError::UnknownId(id.clone()))?;
            selected.push(registration.command());
        }
        Ok(selected)
    }

    pub fn command_ids(&self) -> impl ExactSizeIterator<Item = &'static str> + '_ {
        self.registrations.keys().copied()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandRegistryError {
    InvalidId(String),
    DuplicateId(String),
    DuplicateName(String),
    DuplicateSelection(String),
    UnknownId(String),
}

impl std::fmt::Display for CommandRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidId(id) => write!(f, "invalid command id '{id}'"),
            Self::DuplicateId(id) => write!(f, "duplicate command id '{id}'"),
            Self::DuplicateName(name) => write!(f, "duplicate Nushell command name '{name}'"),
            Self::DuplicateSelection(id) => {
                write!(f, "command id '{id}' was selected more than once")
            }
            Self::UnknownId(id) => write!(f, "unknown command id '{id}'"),
        }
    }
}

impl std::error::Error for CommandRegistryError {}

pub trait WebSearchClient: Send + Sync {
    fn search(
        &self,
        request: kraai_types::WebSearchRequest,
    ) -> Result<kraai_types::WebSearchResponse, String>;
}

pub trait StateEffectClient: Send + Sync {
    fn apply(
        &self,
        command_id: &'static str,
        deltas: Vec<ContextStateDelta>,
    ) -> Result<(), StateEffectError>;
}

pub trait ImageAttachmentClient: Send + Sync {
    fn attach(&self, bytes: Vec<u8>) -> Result<kraai_types::ImageAttachment, String>;
    fn attach_existing(&self, id: String) -> Result<kraai_types::ImageAttachment, String>;
}

#[derive(Clone)]
pub struct CommandContext {
    web_search: Arc<dyn WebSearchClient>,
    state_effects: Arc<dyn StateEffectClient>,
    images: Arc<dyn ImageAttachmentClient>,
}

impl CommandContext {
    pub fn new(
        state_effects: Arc<dyn StateEffectClient>,
        web_search: Arc<dyn WebSearchClient>,
        images: Arc<dyn ImageAttachmentClient>,
    ) -> Self {
        Self {
            state_effects,
            web_search,
            images,
        }
    }

    pub fn images(&self) -> &dyn ImageAttachmentClient {
        self.images.as_ref()
    }

    pub fn web_search(&self) -> &dyn WebSearchClient {
        self.web_search.as_ref()
    }

    pub fn state_effects(&self) -> &dyn StateEffectClient {
        self.state_effects.as_ref()
    }
}

impl std::fmt::Debug for CommandContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandContext").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateEffectError {
    message: String,
}

impl StateEffectError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StateEffectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StateEffectError {}

pub fn command_error(
    title: impl Into<String>,
    message: impl Into<String>,
    span: Span,
) -> ShellError {
    let title: String = title.into();
    let message: String = message.into();
    ShellError::Generic(GenericError::new(title, message, span))
}

#[macro_export]
macro_rules! declare_kraai_command {
    (
        $(#[$attributes:meta])*
        $visibility:vis struct $command:ident;
        metadata: $metadata:expr;
        signature: $signature:expr;
        run: |$context:ident, $engine_state:ident, $stack:ident, $call:ident, $input:ident| $body:block
    ) => {
        $(#[$attributes])*
        #[derive(Clone)]
        $visibility struct $command {
            context: $crate::CommandContext,
        }

        impl $command {
            pub const METADATA: $crate::CommandMetadata = $metadata;

            pub fn new(context: $crate::CommandContext) -> Self {
                Self { context }
            }

            pub fn registration(
                context: $crate::CommandContext,
            ) -> Result<$crate::CommandRegistration, $crate::CommandRegistryError> {
                $crate::CommandRegistration::new(
                    Self::METADATA.id,
                    Self::new(context),
                )
            }
        }

        impl nu_protocol::engine::Command for $command {
            fn name(&self) -> &str {
                Self::METADATA.name
            }

            fn signature(&self) -> nu_protocol::Signature {
                $signature
            }

            fn description(&self) -> &str {
                Self::METADATA.description
            }

            fn examples(&self) -> Vec<nu_protocol::Example<'_>> {
                Self::METADATA
                    .examples
                    .iter()
                    .map(|example| nu_protocol::Example {
                        description: example.description,
                        example: example.script,
                        result: None,
                    })
                    .collect()
            }

            fn run(
                &self,
                engine_state: &nu_protocol::engine::EngineState,
                stack: &mut nu_protocol::engine::Stack,
                call: &nu_protocol::engine::Call,
                input: nu_protocol::PipelineData,
            ) -> Result<nu_protocol::PipelineData, nu_protocol::ShellError> {
                let $context = &self.context;
                let $engine_state = engine_state;
                let $stack = stack;
                let $call = call;
                let $input = input;
                $body
            }
        }
    };
}
