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
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::eyre::ensure;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Result<Self> {
            let directory =
                std::env::temp_dir().join(format!("kraai-eval-harness-{}", ulid::Ulid::generate()));
            fs::create_dir(&directory)?;
            Ok(Self(directory))
        }

        fn executable(&self, name: &str, contents: &str) -> Result<PathBuf> {
            let path = self.0.join(name);
            fs::write(&path, contents)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
            }
            Ok(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn resolves_relative_profile_program_and_preserves_argument_boundaries() -> Result<()> {
        let fixture = Fixture::new()?;
        let executable = fixture.executable("agent with spaces", "#!/bin/sh\nexit 0\n")?;
        let path = fixture.0.join("harness.toml");
        fs::write(
            &path,
            r#"schema_version = 1
name = "custom"
program = "./agent with spaces"
args = ["--model={model}", "{prompt}", "{model}", "{proxy_url}"]
proxy = "openai"
api_key_env = "KRAAI_EVAL_TEST_NONEXISTENT_API_KEY"
"#,
        )?;
        let profile = HarnessProfile::load(&path)?;
        let resolved = profile.resolve("model with spaces", None, None)?;
        ensure!(resolved.program == executable.canonicalize()?);
        ensure!(resolved.name == "custom");
        ensure!(resolved.model_label() == "model with spaces");
        ensure!(
            resolved.args
                == [
                    "--model=model with spaces",
                    "{prompt}",
                    "model with spaces",
                    "{proxy_url}",
                ]
        );
        ensure!(profile.model_proxy(None)?.is_some());
        Ok(())
    }

    #[test]
    fn version_is_the_program_sha256() -> Result<()> {
        let fixture = Fixture::new()?;
        let program = fixture.executable("agent", "abc")?;
        let profile = HarnessProfile::kraai();
        let initial = profile.resolve("test-model", None, Some(&program))?;
        ensure!(
            initial.version == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        fs::write(&program, "#!/bin/sh\nexit 1\n")?;
        let changed = profile.resolve("test-model", None, Some(&program))?;
        ensure!(initial.version != changed.version);
        Ok(())
    }

    #[test]
    fn reasoning_args_come_first_and_only_with_reasoning() -> Result<()> {
        let program = std::env::current_exe()?;
        let profile = HarnessProfile::kraai();
        let plain = profile.resolve("test-model", None, Some(&program))?;
        ensure!(plain.args.first().map(String::as_str) == Some("--provider"));
        ensure!(plain.model_label() == "test-model");
        let high = profile.resolve("test-model", Some("high"), Some(&program))?;
        ensure!(
            high.args.get(..3)
                == Some(
                    &[
                        String::from("--option"),
                        String::from("reasoning_effort=high"),
                        String::from("--provider"),
                    ][..]
                )
        );
        ensure!(high.model_label() == "test-model-high");
        let without = HarnessProfile {
            reasoning_args: Vec::new(),
            ..profile
        };
        ensure!(
            without
                .resolve("test-model", Some("high"), Some(&program))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn resolves_bare_program_on_path() -> Result<()> {
        let profile = HarnessProfile {
            program: PathBuf::from("sh"),
            ..HarnessProfile::kraai()
        };
        let resolved = profile.resolve("test-model", None, None)?;
        ensure!(resolved.program.is_absolute());
        ensure!(is_executable(&resolved.program));
        Ok(())
    }

    #[test]
    fn rejects_invalid_profiles_and_models_before_program_resolution() -> Result<()> {
        let profile = HarnessProfile {
            program: std::env::current_exe()?,
            ..HarnessProfile::kraai()
        };
        for invalid in [
            HarnessProfile {
                schema_version: 2,
                ..profile.clone()
            },
            HarnessProfile {
                name: String::from(" "),
                ..profile.clone()
            },
            HarnessProfile {
                program: PathBuf::new(),
                ..profile.clone()
            },
            HarnessProfile {
                args: vec![String::from("{reasoning}")],
                ..profile.clone()
            },
            HarnessProfile {
                reasoning_args: vec![String::from("--effort")],
                ..profile.clone()
            },
            HarnessProfile {
                max_requests: 0,
                ..profile.clone()
            },
            HarnessProfile {
                proxy: ProxyKind::CodexSubscription,
                ..profile.clone()
            },
            HarnessProfile {
                proxy: ProxyKind::Openai,
                api_key_env: String::from("invalid=key"),
                sanitize_kraai_provider: false,
                ..profile.clone()
            },
        ] {
            ensure!(invalid.resolve("test-model", None, None).is_err());
        }
        ensure!(profile.resolve(" ", None, None).is_err());
        ensure!(profile.resolve("model\0name", None, None).is_err());
        ensure!(profile.resolve("model-{prompt}", None, None).is_err());
        ensure!(profile.resolve("model", Some("{prompt}"), None).is_err());
        Ok(())
    }

    #[test]
    fn rejects_unavailable_runtime_placeholders() -> Result<()> {
        let program = std::env::current_exe()?;
        for placeholder in ["{provider_id}", "{provider_config}", "{proxy_url}"] {
            let profile = HarnessProfile {
                program: program.clone(),
                args: vec![format!("--option={placeholder}")],
                proxy: ProxyKind::None,
                sanitize_kraai_provider: false,
                ..HarnessProfile::kraai()
            };
            ensure!(profile.resolve("test-model", None, None).is_err());
        }
        Ok(())
    }

    #[test]
    fn rejects_unknown_profile_fields() {
        let parsed = toml::from_str::<HarnessProfile>(
            "schema_version = 1\nname = 'agent'\nprogram = 'agent'\nmax_requsts = 3\n",
        );
        assert!(parsed.is_err());
    }

    #[test]
    fn rejects_non_executable_programs() -> Result<()> {
        let fixture = Fixture::new()?;
        let program = fixture.0.join("agent");
        fs::write(&program, "not executable")?;
        let profile = HarnessProfile::kraai();
        ensure!(
            profile
                .resolve("test-model", None, Some(&fixture.0))
                .is_err()
        );
        #[cfg(unix)]
        ensure!(profile.resolve("test-model", None, Some(&program)).is_err());
        ensure!(
            profile
                .resolve("test-model", None, Some(&fixture.0.join("absent")))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn example_profile_matches_builtin() -> Result<()> {
        let example =
            HarnessProfile::load(&crate::eval_assets_directory().join("harnesses/kraai.toml"))?;
        ensure!(serde_json::to_value(&example)? == serde_json::to_value(HarnessProfile::kraai())?);
        ensure!(example.model_proxy(None).is_err());
        Ok(())
    }
}
