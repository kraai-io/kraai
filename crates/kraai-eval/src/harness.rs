use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::cache::sha256_file;
use crate::{KraaiProviderConfigRequest, ModelProxyRequest};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessProfile {
    pub schema_version: u32,
    pub name: String,
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub reasoning_args: Vec<String>,
    #[serde(default)]
    pub proxy: ProxyKind,
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
    #[serde(default = "default_max_requests")]
    pub max_requests: u64,
    #[serde(default)]
    pub sanitize_kraai_provider: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyKind {
    #[default]
    None,
    Openai,
    CodexSubscription,
}

#[derive(Debug, Clone)]
pub struct ResolvedHarness {
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub version: String,
    pub model: String,
    pub reasoning: Option<String>,
}

impl ResolvedHarness {
    pub fn model_label(&self) -> String {
        match &self.reasoning {
            Some(reasoning) => format!("{}-{reasoning}", self.model),
            None => self.model.clone(),
        }
    }
}

impl HarnessProfile {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .wrap_err_with(|| format!("read harness profile {}", path.display()))?;
        let mut profile: Self = toml::from_str(&contents)
            .wrap_err_with(|| format!("parse harness profile {}", path.display()))?;
        profile.validate()?;
        if profile.program.is_relative() && !is_bare_program(&profile.program) {
            let directory = path
                .parent()
                .filter(|directory| !directory.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            profile.program = std::path::absolute(directory)?.join(&profile.program);
        }
        Ok(profile)
    }

    pub fn kraai() -> Self {
        Self {
            schema_version: 1,
            name: String::from("kraai"),
            program: PathBuf::from("kraai-headless"),
            args: [
                "--provider",
                "{provider_id}",
                "--model",
                "{model}",
                "--agent-profile",
                "eval-coding",
                "--message",
                "{prompt}",
                "--provider-config",
                "{provider_config}",
            ]
            .map(String::from)
            .to_vec(),
            reasoning_args: ["--option", "reasoning_effort={reasoning}"]
                .map(String::from)
                .to_vec(),
            proxy: ProxyKind::None,
            api_key_env: default_api_key_env(),
            max_requests: default_max_requests(),
            sanitize_kraai_provider: true,
        }
    }

    pub fn resolve(
        &self,
        model: &str,
        reasoning: Option<&str>,
        program_override: Option<&Path>,
    ) -> Result<ResolvedHarness> {
        self.validate()?;
        let model = template_value("model", model)?;
        let reasoning = reasoning
            .map(|reasoning| template_value("reasoning", reasoning))
            .transpose()?;
        if reasoning.is_some() && self.reasoning_args.is_empty() {
            bail!(
                "harness profile {} has no reasoning_args, so it cannot set --reasoning",
                self.name
            );
        }
        let program = resolve_program(program_override.unwrap_or(&self.program))?;
        let reasoning_args = reasoning.map_or_else(Vec::new, |reasoning| {
            self.reasoning_args
                .iter()
                .map(|arg| arg.replace("{reasoning}", reasoning))
                .collect()
        });
        Ok(ResolvedHarness {
            name: self.name.trim().to_owned(),
            version: sha256_file(&program)?,
            program,
            args: reasoning_args
                .iter()
                .chain(&self.args)
                .map(|arg| arg.replace("{model}", model))
                .collect(),
            model: model.to_owned(),
            reasoning: reasoning.map(str::to_owned),
        })
    }

    pub fn model_proxy(
        &self,
        provider: Option<&KraaiProviderConfigRequest>,
    ) -> Result<Option<ModelProxyRequest>> {
        if self.sanitize_kraai_provider {
            let Some(provider) = provider else {
                bail!("a Kraai profile needs a provider config to choose its proxy");
            };
            return ModelProxyRequest::kraai_provider(provider.clone(), self.max_requests)
                .map(Some);
        }
        Ok(match self.proxy {
            ProxyKind::None => None,
            ProxyKind::Openai => Some(ModelProxyRequest::openai(
                self.api_key_env.clone(),
                self.max_requests,
            )),
            ProxyKind::CodexSubscription => {
                Some(ModelProxyRequest::codex_subscription(self.max_requests))
            }
        })
    }

    pub fn has_model_proxy(&self) -> bool {
        self.sanitize_kraai_provider || self.proxy != ProxyKind::None
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!(
                "unsupported harness schema version: {}",
                self.schema_version
            );
        }
        if self.name.trim().is_empty() || self.name.contains('\0') {
            bail!("harness name must not be empty or contain a null byte");
        }
        if self.program.as_os_str().is_empty() {
            bail!("harness program must not be empty");
        }
        if self
            .args
            .iter()
            .chain(&self.reasoning_args)
            .any(|arg| arg.contains('\0'))
        {
            bail!("harness arguments must not contain null bytes");
        }
        if self.args.iter().any(|arg| arg.contains("{reasoning}")) {
            bail!("put {{reasoning}} in reasoning_args, which are only passed with --reasoning");
        }
        if !self.reasoning_args.is_empty()
            && !self
                .reasoning_args
                .iter()
                .any(|arg| arg.contains("{reasoning}"))
        {
            bail!("reasoning_args must use the {{reasoning}} placeholder");
        }
        if self.max_requests == 0 {
            bail!("harness max_requests must be greater than zero");
        }
        if self.proxy == ProxyKind::Openai && !valid_environment_name(&self.api_key_env) {
            bail!("harness api_key_env must be a valid environment variable name");
        }
        if self.sanitize_kraai_provider && self.proxy != ProxyKind::None {
            bail!("Kraai profiles take their proxy from the selected provider; remove proxy");
        }
        if !self.sanitize_kraai_provider
            && self
                .args
                .iter()
                .any(|arg| arg.contains("{provider_id}") || arg.contains("{provider_config}"))
        {
            bail!("provider placeholders require sanitize_kraai_provider = true");
        }
        if !self.has_model_proxy() && self.args.iter().any(|arg| arg.contains("{proxy_url}")) {
            bail!("the proxy_url placeholder requires a model proxy");
        }
        Ok(())
    }
}

fn template_value<'a>(name: &str, value: &'a str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() || value.contains('\0') {
        bail!("harness {name} must not be empty or contain a null byte");
    }
    if value.contains(['{', '}']) {
        bail!("harness {name} must not contain braces");
    }
    Ok(value)
}

fn default_api_key_env() -> String {
    String::from("OPENAI_API_KEY")
}

fn default_max_requests() -> u64 {
    64
}

fn valid_environment_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn is_bare_program(program: &Path) -> bool {
    program.components().count() == 1 && program.file_name().is_some()
}

fn resolve_program(program: &Path) -> Result<PathBuf> {
    if is_bare_program(program) {
        let path = std::env::var_os("PATH")
            .ok_or_else(|| color_eyre::eyre::eyre!("PATH is unavailable"))?;
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(program);
            if is_executable(&candidate) {
                return candidate
                    .canonicalize()
                    .wrap_err_with(|| format!("resolve harness program {}", candidate.display()));
            }
        }
        bail!("harness program not found on PATH: {}", program.display());
    }
    if !is_executable(program) {
        bail!(
            "harness program is missing or not executable: {}",
            program.display()
        );
    }
    program
        .canonicalize()
        .wrap_err_with(|| format!("resolve harness program {}", program.display()))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

#[cfg(test)]
mod tests;
