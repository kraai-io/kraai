use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, eyre};

pub(super) struct NushellHost {
    pub(super) executable: PathBuf,
    pub(super) arguments: Vec<std::ffi::OsString>,
}

pub(super) fn resolve_nushell_host(use_current_executable: bool) -> Result<NushellHost> {
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
    if host.try_exists()? {
        let executable = canonical_executable(&host)?;
        return Ok(NushellHost {
            executable,
            arguments: Vec::new(),
        });
    }
    if use_current_executable {
        return Ok(NushellHost {
            executable: current_executable,
            arguments: vec![kraai_nushell_runtime::INTERNAL_HOST_ARGUMENT.into()],
        });
    }
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
    fn nushell_host_prefers_packaged_sibling_and_can_fall_back_to_frontend() -> Result<()> {
        let directory =
            std::env::temp_dir().join(format!("kraai-host-resolution-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&directory)?;
        let frontend = directory.join("kraai");
        std::fs::write(&frontend, [])?;
        let frontend = frontend.canonicalize()?;

        let fallback = resolve_nushell_host_from(frontend.clone(), true)?;
        if fallback.executable != frontend {
            return Err(eyre!("frontend fallback selected the wrong executable"));
        }
        if fallback.arguments
            != [std::ffi::OsString::from(
                kraai_nushell_runtime::INTERNAL_HOST_ARGUMENT,
            )]
        {
            return Err(eyre!(
                "frontend fallback omitted the internal host argument"
            ));
        }
        if resolve_nushell_host_from(frontend.clone(), false).is_ok() {
            return Err(eyre!("frontend fallback was enabled without opt-in"));
        }

        let packaged = directory.join(format!(
            "kraai-nushell-host{}",
            std::env::consts::EXE_SUFFIX
        ));
        std::fs::write(&packaged, [])?;
        let packaged = packaged.canonicalize()?;
        let resolved = resolve_nushell_host_from(frontend, true)?;
        if resolved.executable != packaged || !resolved.arguments.is_empty() {
            return Err(eyre!("packaged host was not preferred over the fallback"));
        }

        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
