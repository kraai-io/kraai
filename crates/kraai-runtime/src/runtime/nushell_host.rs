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
mod tests;
