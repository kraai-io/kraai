use std::io::{Result, Write};
use std::sync::atomic::{AtomicU8, Ordering};

use ratatui::crossterm::{cursor::SetCursorStyle, execute};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorStyle(u8);

impl CursorStyle {
    pub(crate) fn from_report(value: u8) -> Option<Self> {
        (1..=6).contains(&value).then_some(Self(value))
    }

    pub(crate) fn has_unambiguous_blinking_shape(self) -> bool {
        matches!(self.0, 3 | 5)
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
    pub(crate) fn apply(&self, style: Option<CursorStyle>) -> Result<()> {
        self.apply_with(style, &mut std::io::stdout())
    }

    fn apply_with(&self, style: Option<CursorStyle>, output: &mut impl Write) -> Result<()> {
        let Some(style) = style else { return Ok(()) };
        if !style.has_unambiguous_blinking_shape() || self.original.load(Ordering::Relaxed) != 0 {
            return Ok(());
        }
        self.original.store(style.0, Ordering::Relaxed);
        execute!(output, style.steady().command())
    }

    pub(crate) fn restore(&self) -> Result<()> {
        self.restore_with(&mut std::io::stdout())
    }

    fn restore_with(&self, output: &mut impl Write) -> Result<()> {
        let Some(style) = CursorStyle::from_report(self.original.load(Ordering::Relaxed)) else {
            return Ok(());
        };
        execute!(output, style.command())?;
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
            assert_eq!(
                style.map(CursorStyle::has_unambiguous_blinking_shape),
                Some(matches!(code, 3 | 5))
            );
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
    fn unknown_or_default_style_is_never_overridden() {
        for code in [0, 7, 255] {
            let cursor = CursorOverride::default();
            let mut output = Vec::new();
            assert!(
                cursor
                    .apply_with(CursorStyle::from_report(code), &mut output)
                    .is_ok()
            );
            assert!(cursor.restore_with(&mut output).is_ok());
            assert!(output.is_empty());
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
        for write in [false, true] {
            let cursor = CursorOverride::default();
            let mut failing = Fails { write };
            assert!(
                cursor
                    .apply_with(CursorStyle::from_report(5), &mut failing)
                    .is_err()
            );
            assert!(cursor.restore_with(&mut failing).is_err());
            let mut output = Vec::new();
            assert!(cursor.restore_with(&mut output).is_ok());
            assert_eq!(output, b"\x1b[5 q");
        }
    }
}
