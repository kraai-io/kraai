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
        if name.is_empty() || name.contains(&u16::from(b'=')) || name.len() > 32767 {
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
mod tests {
    use super::*;

    #[test]
    fn quotes_empty_whitespace_quotes_and_trailing_backslashes() {
        for (argument, expected) in [
            ("", "\"\""),
            ("one two", "\"one two\""),
            ("a\"b", "\"a\\\"b\""),
            ("tail\\", "\"tail\\\\\""),
            ("a\\\"b", "\"a\\\\\\\"b\""),
        ] {
            let mut encoded = Vec::new();
            assert!(quote(&mut encoded, OsStr::new(argument)).is_ok());
            assert_eq!(String::from_utf16_lossy(&encoded), expected);
        }
    }

    #[test]
    fn environment_rejects_case_aliases_and_preserves_empty_block() {
        assert_eq!(environment(&BTreeMap::new()).ok(), Some(vec![0, 0]));
        let values = BTreeMap::from([
            (OsString::from("Path"), OsString::from("first")),
            (OsString::from("PATH"), OsString::from("second")),
        ]);
        assert!(environment(&values).is_err());
        assert!(wide(OsStr::new("nul\0value")).is_err());
    }

    #[test]
    fn rejects_shell_scripts_and_invalid_command_lines() {
        assert!(application(Path::new(r"C:\tools\script.CmD")).is_err());
        assert!(application(Path::new(r"C:\tools\script.bat")).is_err());
        assert!(application(Path::new("relative.exe")).is_err());
        assert!(
            arguments(
                Path::new(r"C:\tool.exe"),
                &[OsString::from("nul\0argument")]
            )
            .is_err()
        );
        assert!(
            arguments(
                Path::new(r"C:\tool.exe"),
                &[OsString::from("a".repeat(32767))]
            )
            .is_err()
        );
    }

    #[test]
    fn preserves_environment_values_and_orders_names() {
        let values = BTreeMap::from([
            (OsString::from("zed"), OsString::from("spaces and = signs")),
            (OsString::from("Alpha"), OsString::from("\u{1f426}")),
        ]);
        assert_eq!(
            environment(&values)
                .map(|block| String::from_utf16_lossy(&block))
                .ok(),
            Some(String::from("Alpha=\u{1f426}\0zed=spaces and = signs\0\0"))
        );
    }
}
