use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::ModelProxyRequest;
use crate::cache::hash_file;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessProfile {
    pub schema_version: u32,
    pub name: String,
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    pub version: Option<String>,
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
    pub model_label: String,
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
            program: PathBuf::from("kraai"),
            args: [
                "--ci",
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
            version: None,
            proxy: ProxyKind::CodexSubscription,
            api_key_env: default_api_key_env(),
            max_requests: default_max_requests(),
            sanitize_kraai_provider: true,
        }
    }

    pub fn resolve(&self, model: &str, program_override: Option<&Path>) -> Result<ResolvedHarness> {
        self.validate()?;
        let model = model.trim();
        if model.trim().is_empty() || model.contains('\0') {
            bail!("harness model must not be empty or contain a null byte");
        }
        if model.contains(['{', '}']) {
            bail!("harness model must not contain braces");
        }
        let program = resolve_program(program_override.unwrap_or(&self.program))?;
        let version = match &self.version {
            Some(version) => version.clone(),
            None => format!("sha256:{}", hash_file(&program)?),
        };
        Ok(ResolvedHarness {
            name: self.name.trim().to_owned(),
            program,
            args: self
                .args
                .iter()
                .map(|arg| arg.replace("{model}", model))
                .collect(),
            version,
            model_label: model.to_owned(),
        })
    }

    pub fn model_proxy(&self) -> Option<ModelProxyRequest> {
        match self.proxy {
            ProxyKind::None => None,
            ProxyKind::Openai => Some(ModelProxyRequest::openai(
                self.api_key_env.clone(),
                self.max_requests,
            )),
            ProxyKind::CodexSubscription => {
                Some(ModelProxyRequest::codex_subscription(self.max_requests))
            }
        }
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
        if self.args.iter().any(|arg| arg.contains('\0')) {
            bail!("harness arguments must not contain null bytes");
        }
        if self
            .version
            .as_ref()
            .is_some_and(|version| version.trim().is_empty() || version.contains('\0'))
        {
            bail!("harness version must not be empty or contain a null byte");
        }
        if self.max_requests == 0 {
            bail!("harness max_requests must be greater than zero");
        }
        if self.proxy == ProxyKind::Openai && !valid_environment_name(&self.api_key_env) {
            bail!("harness api_key_env must be a valid environment variable name");
        }
        if self.sanitize_kraai_provider && self.proxy != ProxyKind::CodexSubscription {
            bail!("sanitizing a Kraai provider requires the codex_subscription proxy");
        }
        if !self.sanitize_kraai_provider
            && self
                .args
                .iter()
                .any(|arg| arg.contains("{provider_id}") || arg.contains("{provider_config}"))
        {
            bail!("provider placeholders require sanitize_kraai_provider = true");
        }
        if self.proxy == ProxyKind::None && self.args.iter().any(|arg| arg.contains("{proxy_url}"))
        {
            bail!("the proxy_url placeholder requires a model proxy");
        }
        Ok(())
    }
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
        let resolved = profile.resolve("model with spaces", None)?;
        ensure!(resolved.program == executable.canonicalize()?);
        ensure!(resolved.name == "custom");
        ensure!(resolved.model_label == "model with spaces");
        ensure!(
            resolved.args
                == [
                    "--model=model with spaces",
                    "{prompt}",
                    "model with spaces",
                    "{proxy_url}",
                ]
        );
        ensure!(profile.model_proxy().is_some());
        Ok(())
    }

    #[test]
    fn artifact_version_changes_when_program_changes() -> Result<()> {
        let fixture = Fixture::new()?;
        let program = fixture.executable("agent", "#!/bin/sh\nexit 0\n")?;
        let profile = HarnessProfile::kraai();
        let initial = profile.resolve("test-model", Some(&program))?;
        fs::write(&program, "#!/bin/sh\nexit 1\n")?;
        let changed = profile.resolve("test-model", Some(&program))?;
        ensure!(initial.version != changed.version);
        ensure!(changed.version.starts_with("sha256:"));
        let pinned = HarnessProfile {
            version: Some(String::from("v1.2.3")),
            ..profile
        };
        ensure!(pinned.resolve("test-model", Some(&program))?.version == "v1.2.3");
        Ok(())
    }

    #[test]
    fn resolves_bare_program_on_path() -> Result<()> {
        let profile = HarnessProfile {
            program: PathBuf::from("sh"),
            ..HarnessProfile::kraai()
        };
        let resolved = profile.resolve("test-model", None)?;
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
                version: Some(String::new()),
                ..profile.clone()
            },
            HarnessProfile {
                max_requests: 0,
                ..profile.clone()
            },
            HarnessProfile {
                proxy: ProxyKind::None,
                ..profile.clone()
            },
            HarnessProfile {
                proxy: ProxyKind::Openai,
                api_key_env: String::from("invalid=key"),
                sanitize_kraai_provider: false,
                ..profile.clone()
            },
        ] {
            ensure!(invalid.resolve("test-model", None).is_err());
        }
        ensure!(profile.resolve(" ", None).is_err());
        ensure!(profile.resolve("model\0name", None).is_err());
        ensure!(profile.resolve("model-{prompt}", None).is_err());
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
            ensure!(profile.resolve("test-model", None).is_err());
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
        ensure!(profile.resolve("test-model", Some(&fixture.0)).is_err());
        #[cfg(unix)]
        ensure!(profile.resolve("test-model", Some(&program)).is_err());
        ensure!(
            profile
                .resolve("test-model", Some(&fixture.0.join("absent")))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn example_profile_matches_builtin() -> Result<()> {
        let example =
            HarnessProfile::load(&crate::eval_assets_directory().join("harnesses/kraai.toml"))?;
        ensure!(serde_json::to_value(&example)? == serde_json::to_value(HarnessProfile::kraai())?);
        Ok(())
    }
}
