use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use kraai_execution_types::NushellStartup;
use nu_config::EnvAccess;
use serde::{Deserialize, Serialize};

/// A Nushell startup file as the script's shell loads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupFile {
    pub path: PathBuf,
    pub contents: Vec<u8>,
}

/// Reads `env.nu` and `config.nu` in load order for a script that runs with `environment`, or
/// nothing for a clean startup. The runtime reads them once, so the host loads the same bytes
/// that the bash-habits rewrite parsed.
pub fn startup_files(
    startup: NushellStartup,
    environment: &BTreeMap<String, String>,
) -> Result<Vec<StartupFile>, String> {
    if startup != NushellStartup::Inherit {
        return Ok(Vec::new());
    }
    let (dirs, _warnings) = nu_config::resolve_paths(
        &ScriptEnvironment(environment),
        &nu_config::CliOverrides::default(),
    )
    .map_err(|error| format!("unable to resolve startup paths: {error}"))?;
    let mut files = Vec::new();
    for path in [dirs.env_file.to_path_buf(), dirs.config_file.to_path_buf()] {
        let contents = kraai_io::fs::read_optional(&path).map_err(|error| {
            format!("unable to read startup file '{}': {error}", path.display())
        })?;
        if let Some(contents) = contents {
            files.push(StartupFile { path, contents });
        }
    }
    Ok(files)
}

/// The script's environment variables, with this platform's directories as fallbacks.
struct ScriptEnvironment<'a>(&'a BTreeMap<String, String>);

impl EnvAccess for ScriptEnvironment<'_> {
    fn var_os(&self, name: &str) -> Option<OsString> {
        self.0.get(name).map(OsString::from)
    }

    fn config_dir(&self) -> Option<PathBuf> {
        nu_config::SystemEnv.config_dir()
    }

    fn data_dir(&self) -> Option<PathBuf> {
        nu_config::SystemEnv.data_dir()
    }

    fn cache_dir(&self) -> Option<PathBuf> {
        nu_config::SystemEnv.cache_dir()
    }

    fn home_dir(&self) -> Option<PathBuf> {
        nu_config::SystemEnv.home_dir()
    }

    #[cfg(windows)]
    fn program_data_dir(&self) -> Option<PathBuf> {
        nu_config::SystemEnv.program_data_dir()
    }
}
