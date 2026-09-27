use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, eyre};

pub(super) struct NushellHost {
    pub(super) executable: PathBuf,
    pub(super) arguments: Vec<std::ffi::OsString>,
}

pub(super) fn resolve_nushell_host(
    explicit_path: Option<&Path>,
    use_current_executable: bool,
) -> Result<NushellHost> {
    if let Some(path) = explicit_path {
        return Ok(NushellHost {
            executable: canonical_executable(path)?,
            arguments: Vec::new(),
        });
    }
    let current_executable = std::env::current_exe()
        .context("Failed to locate the running Kraai executable")?
        .canonicalize()
        .context("Failed to canonicalize the running Kraai executable")?;
    resolve_nushell_host_from(current_executable, use_current_executable)
}

fn resolve_nushell_host_from(
    current_executable: PathBuf,
    use_current_executable: bool,
) -> Result<NushellHost> {
    if use_current_executable {
        return Ok(NushellHost {
            executable: current_executable,
            arguments: vec![kraai_nushell_runtime::INTERNAL_HOST_ARGUMENT.into()],
        });
    }
    let directory = current_executable.parent().ok_or_else(|| {
        eyre!(
            "Kraai executable has no parent directory: {}",
            current_executable.display()
        )
    })?;
    let host = directory.join(format!(
        "kraai-nushell-host{}",
        std::env::consts::EXE_SUFFIX
    ));
    canonical_executable(&host)
        .map(|executable| NushellHost {
            executable,
            arguments: Vec::new(),
        })
        .with_context(|| {
            format!(
                "Unable to locate the packaged Nushell host beside Kraai at {}",
                host.display()
            )
        })
}

fn canonical_executable(path: &Path) -> Result<PathBuf> {
    let canonical = path.canonicalize()?;
    if !canonical.is_file() {
        return Err(eyre!("Nushell host is not a file: {}", canonical.display()));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nushell_host_uses_frontend_when_enabled_and_external_host_otherwise() -> Result<()> {
        let directory =
            std::env::temp_dir().join(format!("kraai-host-resolution-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&directory)?;
        let frontend = directory.join("kraai");
        std::fs::write(&frontend, [])?;
        let frontend = frontend.canonicalize()?;

        let embedded = resolve_nushell_host_from(frontend.clone(), true)?;
        if embedded.executable != frontend {
            return Err(eyre!("frontend host selected the wrong executable"));
        }
        if embedded.arguments
            != [std::ffi::OsString::from(
                kraai_nushell_runtime::INTERNAL_HOST_ARGUMENT,
            )]
        {
            return Err(eyre!("frontend host omitted the internal host argument"));
        }
        if resolve_nushell_host_from(frontend.clone(), false).is_ok() {
            return Err(eyre!("frontend host was enabled without opt-in"));
        }

        let packaged = directory.join(format!(
            "kraai-nushell-host{}",
            std::env::consts::EXE_SUFFIX
        ));
        std::fs::write(&packaged, [])?;
        let packaged = packaged.canonicalize()?;
        let resolved = resolve_nushell_host_from(frontend.clone(), true)?;
        if resolved.executable != embedded.executable || resolved.arguments != embedded.arguments {
            return Err(eyre!("packaged host overrode the frontend host"));
        }
        let resolved = resolve_nushell_host_from(frontend, false)?;
        if resolved.executable != packaged || !resolved.arguments.is_empty() {
            return Err(eyre!("packaged host was not selected"));
        }

        let explicit = directory.join("custom-host");
        std::fs::write(&explicit, [])?;
        let resolved = resolve_nushell_host(Some(&explicit), false)?;
        if resolved.executable != explicit.canonicalize()? || !resolved.arguments.is_empty() {
            return Err(eyre!("explicit host was not selected"));
        }
        if resolve_nushell_host(Some(&directory.join("missing-host")), true).is_ok() {
            return Err(eyre!("invalid explicit host silently fell back"));
        }

        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
