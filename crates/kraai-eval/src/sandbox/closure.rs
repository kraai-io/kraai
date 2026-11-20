use std::ffi::OsStr;
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::eyre::{Result, bail};
use object::Endianness;
use object::elf::{DT_RPATH, DT_RUNPATH, FileHeader64};
use object::read::ReadCache;
use object::read::elf::{Dyn, FileHeader, ProgramHeader};

use super::find_program;

const NIX_STORE: &str = "/nix/store";

pub(super) fn runtime_roots(program: &Path) -> Result<Vec<PathBuf>> {
    if program.starts_with(NIX_STORE) {
        return nix_closure(&[program.to_path_buf()]);
    }
    linked_roots(program)
}

pub(super) fn linked_roots(program: &Path) -> Result<Vec<PathBuf>> {
    let paths = linked_store_paths(program)?;
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    nix_closure(&paths)
}

fn linked_store_paths(program: &Path) -> Result<Vec<PathBuf>> {
    let data = ReadCache::new(File::open(program)?);
    let Ok(header) = FileHeader64::<Endianness>::parse(&data) else {
        return Ok(Vec::new());
    };
    let endian = header.endian()?;
    let mut paths = Vec::new();
    for segment in header.program_headers(endian, &data)? {
        if let Some(interpreter) = segment.interpreter(endian, &data)? {
            paths.push(PathBuf::from(OsStr::from_bytes(interpreter)));
        }
    }
    let sections = header.sections(endian, &data)?;
    if let Some((entries, strings)) = sections.dynamic(endian, &data)? {
        let strings = sections.strings(endian, &data, strings)?;
        for entry in entries {
            if !matches!(entry.tag32(endian), Some(DT_RUNPATH | DT_RPATH)) {
                continue;
            }
            let offset = entry.d_val(endian);
            let search_path = strings
                .get(u32::try_from(offset)?)
                .map_err(|()| color_eyre::eyre::eyre!("invalid ELF library search path"))?;
            paths.extend(
                search_path
                    .split(|byte| *byte == b':')
                    .map(|directory| PathBuf::from(OsStr::from_bytes(directory))),
            );
        }
    }
    paths.retain(|path| path.starts_with(NIX_STORE) && path.exists());
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn nix_closure(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let nix_store = find_program("nix-store").ok_or_else(|| {
        color_eyre::eyre::eyre!("nix-store is required to isolate a Nix runner closure")
    })?;
    let output = Command::new(nix_store)
        .args(["--query", "--requisites"])
        .args(paths)
        .output()?;
    if !output.status.success() {
        bail!(
            "failed to resolve runner Nix closure: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut roots = String::from_utf8(output.stdout)?
        .lines()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if roots.is_empty()
        || roots
            .iter()
            .any(|root| !root.starts_with(NIX_STORE) || !root.exists())
    {
        bail!("nix-store returned an invalid runner closure");
    }
    roots.sort();
    roots.dedup();
    Ok(roots)
}
