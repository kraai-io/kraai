use std::io::{Result, Write};
use std::sync::atomic::{AtomicU8, Ordering};

use ratatui::crossterm::{cursor::SetCursorStyle, execute};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorStyle(u8);

impl CursorStyle {
    pub(crate) fn from_report(value: u8) -> Option<Self> {
        (1..=6).contains(&value).then_some(Self(value))
    }

    pub(crate) fn custom_blink(style: Option<Self>) -> bool {
        style.is_none_or(|style| matches!(style.0, 3 | 5))
    }

    fn command(self) -> SetCursorStyle {
        match self.0 {
            1 => SetCursorStyle::BlinkingBlock,
            2 => SetCursorStyle::SteadyBlock,
            3 => SetCursorStyle::BlinkingUnderScore,
            4 => SetCursorStyle::SteadyUnderScore,
            5 => SetCursorStyle::BlinkingBar,
            _ => SetCursorStyle::SteadyBar,
        }
    }

    fn steady(self) -> Self {
        Self(self.0 + self.0 % 2)
    }
}

#[derive(Default)]
pub(crate) struct CursorOverride {
    original: AtomicU8,
}

impl CursorOverride {
    const UNKNOWN_ORIGINAL: u8 = 7;

    pub(crate) fn apply(&self, style: Option<CursorStyle>) -> Result<()> {
        self.apply_with(style, &mut std::io::stdout())
    }

    fn apply_with(&self, style: Option<CursorStyle>, output: &mut impl Write) -> Result<()> {
        let original = self.original.load(Ordering::Relaxed);
        if original != 0 && original != Self::UNKNOWN_ORIGINAL {
            return Ok(());
        }
        let next_original = style.map_or(Self::UNKNOWN_ORIGINAL, |style| style.0);
        if !CursorStyle::custom_blink(style) {
            if original == 0 {
                return Ok(());
            }
            self.original.store(next_original, Ordering::Relaxed);
            return self.restore_with(output);
        }
        if original == next_original {
            return Ok(());
        }
        self.original.store(next_original, Ordering::Relaxed);
        execute!(output, style.unwrap_or(CursorStyle(5)).steady().command())
    }

    pub(crate) fn restore(&self) -> Result<()> {
        self.restore_with(&mut std::io::stdout())
    }

    fn restore_with(&self, output: &mut impl Write) -> Result<()> {
        let command = match self.original.load(Ordering::Relaxed) {
            Self::UNKNOWN_ORIGINAL => SetCursorStyle::DefaultUserShape,
            value => match CursorStyle::from_report(value) {
                Some(style) => style.command(),
                None => return Ok(()),
            },
        };
        execute!(output, command)?;
        self.original.store(0, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_every_reported_shape_and_blink_preference() {
        for code in 1..=6 {
            let style = CursorStyle::from_report(code);
            assert_eq!(CursorStyle::custom_blink(style), matches!(code, 3 | 5));
            let cursor = CursorOverride::default();
            let mut output = Vec::new();
            assert!(cursor.apply_with(style, &mut output).is_ok());
            if !matches!(code, 3 | 5) {
                assert!(output.is_empty());
                assert!(cursor.restore_with(&mut output).is_ok());
                assert!(output.is_empty());
                continue;
            }
            assert_eq!(output, format!("\x1b[{} q", code + code % 2).as_bytes());
            output.clear();
            assert!(cursor.apply_with(style, &mut output).is_ok());
            assert!(output.is_empty());
            assert!(cursor.restore_with(&mut output).is_ok());
            assert_eq!(output, format!("\x1b[{code} q").as_bytes());
            output.clear();
            assert!(cursor.restore_with(&mut output).is_ok());
            assert!(output.is_empty());
            assert!(cursor.apply_with(style, &mut output).is_ok());
            assert_eq!(output, format!("\x1b[{} q", code + code % 2).as_bytes());
        }
    }

    #[test]
    fn unknown_or_default_style_uses_bar_and_restores_terminal_default() {
        for code in [0, 7, 255] {
            let cursor = CursorOverride::default();
            let mut output = Vec::new();
            assert!(
                cursor
                    .apply_with(CursorStyle::from_report(code), &mut output)
                    .is_ok()
            );
            assert_eq!(output, b"\x1b[6 q");
            output.clear();
            assert!(cursor.apply_with(None, &mut output).is_ok());
            assert!(output.is_empty());
            assert!(cursor.restore_with(&mut output).is_ok());
            assert_eq!(output, b"\x1b[0 q");
            output.clear();
            assert!(cursor.restore_with(&mut output).is_ok());
            assert!(output.is_empty());
            assert!(cursor.apply_with(None, &mut output).is_ok());
            assert_eq!(output, b"\x1b[6 q");
        }
    }

    #[test]
    fn late_reports_replace_fallback_and_restore_detected_preferences() {
        for code in 1..=6 {
            let cursor = CursorOverride::default();
            let mut output = Vec::new();
            assert!(cursor.apply_with(None, &mut output).is_ok());
            output.clear();
            let style = CursorStyle::from_report(code);
            assert!(cursor.apply_with(style, &mut output).is_ok());
            let applied = if matches!(code, 3 | 5) {
                code + 1
            } else {
                code
            };
            assert_eq!(output, format!("\x1b[{applied} q").as_bytes());
            output.clear();
            assert!(cursor.apply_with(style, &mut output).is_ok());
            assert!(output.is_empty());
            assert!(cursor.restore_with(&mut output).is_ok());
            if matches!(code, 3 | 5) {
                assert_eq!(output, format!("\x1b[{code} q").as_bytes());
            } else {
                assert!(output.is_empty());
            }
        }
    }

    #[test]
    fn failed_late_report_writes_retain_detected_style_for_cleanup() {
        for code in 1..=6 {
            let cursor = CursorOverride::default();
            assert!(cursor.apply_with(None, &mut Vec::new()).is_ok());
            let mut output = [0; 0];
            assert!(
                cursor
                    .apply_with(CursorStyle::from_report(code), &mut output.as_mut_slice())
                    .is_err()
            );
            let mut output = Vec::new();
            assert!(cursor.restore_with(&mut output).is_ok());
            assert_eq!(output, format!("\x1b[{code} q").as_bytes());
        }
    }

    #[test]
    fn failed_writes_retain_the_original_style_for_cleanup() {
        struct Fails {
            write: bool,
        }
        impl Write for Fails {
            fn write(&mut self, bytes: &[u8]) -> Result<usize> {
                if self.write {
                    Err(std::io::Error::other("write failed"))
                } else {
                    Ok(bytes.len())
                }
            }
            fn flush(&mut self) -> Result<()> {
                Err(std::io::Error::other("flush failed"))
            }
        }
        for (write, code) in [false, true]
            .into_iter()
            .flat_map(|write| [0, 5].map(|code| (write, code)))
        {
            let cursor = CursorOverride::default();
            let mut failing = Fails { write };
            assert!(
                cursor
                    .apply_with(CursorStyle::from_report(code), &mut failing)
                    .is_err()
            );
            assert!(cursor.restore_with(&mut failing).is_err());
            let mut output = Vec::new();
            assert!(cursor.restore_with(&mut output).is_ok());
            assert_eq!(output, format!("\x1b[{code} q").as_bytes());
        }
    }
}
