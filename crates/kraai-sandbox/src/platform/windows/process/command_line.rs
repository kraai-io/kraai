use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Globalization::CompareStringOrdinal;

pub(super) fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value = value.encode_wide().collect::<Vec<_>>();
    if value.contains(&0) {
        return Err(invalid("Windows process strings must not contain NUL"));
    }
    value.push(0);
    Ok(value)
}

pub(super) fn application(path: &Path) -> io::Result<Vec<u16>> {
    if !path.is_absolute() {
        return Err(invalid("Windows executable path must be absolute"));
    }
    if path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
    }) {
        return Err(invalid(
            "batch files cannot be launched directly; specify an executable",
        ));
    }
    wide(path.as_os_str())
}

pub(super) fn arguments(executable: &Path, args: &[OsString]) -> io::Result<Vec<u16>> {
    let mut command = Vec::new();
    for argument in
        std::iter::once(executable.as_os_str()).chain(args.iter().map(OsString::as_os_str))
    {
        if !command.is_empty() {
            command.push(u16::from(b' '));
        }
        quote(&mut command, argument)?;
    }
    command.push(0);
    if command.len() > 32767 {
        return Err(invalid(
            "Windows command line exceeds 32767 UTF-16 code units",
        ));
    }
    Ok(command)
}

fn quote(output: &mut Vec<u16>, argument: &OsStr) -> io::Result<()> {
    output.push(u16::from(b'"'));
    let mut backslashes = 0;
    for character in argument.encode_wide() {
        match character {
            0 => return Err(invalid("Windows arguments must not contain NUL")),
            92 => backslashes += 1,
            34 => {
                output.extend(std::iter::repeat_n(92, backslashes * 2 + 1));
                output.push(character);
                backslashes = 0;
            }
            _ => {
                output.extend(std::iter::repeat_n(92, backslashes));
                output.push(character);
                backslashes = 0;
            }
        }
    }
    output.extend(std::iter::repeat_n(92, backslashes * 2));
    output.push(u16::from(b'"'));
    Ok(())
}

pub(super) fn environment(environment: &BTreeMap<OsString, OsString>) -> io::Result<Vec<u16>> {
    let mut entries = Vec::with_capacity(environment.len());
    for (name, value) in environment {
        let mut name = wide(name)?;
        name.pop();
        let visible_name = name.strip_prefix(&[u16::from(b'=')]).unwrap_or(&name);
        if visible_name.is_empty() || visible_name.contains(&u16::from(b'=')) || name.len() > 32767
        {
            return Err(invalid("invalid Windows environment variable name"));
        }
        entries.push((name, wide(value)?));
    }
    entries.sort_by(|(left, _), (right, _)| compare_names(left, right));
    for pair in entries.windows(2) {
        if let [(left, _), (right, _)] = pair
            && compare_names(left, right) == Ordering::Equal
        {
            return Err(invalid(
                "Windows environment contains duplicate names differing only by case",
            ));
        }
    }
    let mut block = Vec::new();
    for (name, value) in entries {
        block.extend(name);
        block.push(u16::from(b'='));
        block.extend(value);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

#[expect(
    unsafe_code,
    reason = "Windows environment names use ordinal case-insensitive ordering"
)]
fn compare_names(left: &[u16], right: &[u16]) -> Ordering {
    unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32,
            right.as_ptr(),
            right.len() as i32,
            1,
        )
    }
    .cmp(&2)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests;
