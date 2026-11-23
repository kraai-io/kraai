use std::io::{Result, Write};
use std::sync::atomic::{AtomicU8, Ordering};

use ratatui::crossterm::{cursor::SetCursorStyle, execute};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorStyle(u8);

impl CursorStyle {
    pub(crate) const fn from_report(value: u8) -> Option<Self> {
        match value {
            1..=6 => Some(Self(value)),
            _ => None,
        }
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

const _: () = assert!(CursorStyle::from_report(CursorOverride::UNKNOWN_ORIGINAL).is_none());

impl CursorOverride {
    const UNKNOWN_ORIGINAL: u8 = 7;

    pub(crate) fn apply(&self, style: Option<CursorStyle>) -> Result<()> {
        self.apply_with(style, &mut std::io::stdout())
    }

    fn apply_with(&self, style: Option<CursorStyle>, output: &mut impl Write) -> Result<()> {
        // Serialize state and output with restore; stdout's lock allows same-thread panic cleanup.
        let _output_lock = std::io::stdout().lock();
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
        // Retain the original before writing so callers can clean up after an error.
        // Reapplying the same style does not retry a failed write.
        self.original.store(next_original, Ordering::Relaxed);
        execute!(output, style.unwrap_or(CursorStyle(5)).steady().command())
    }

    pub(crate) fn restore(&self) -> Result<()> {
        self.restore_with(&mut std::io::stdout())
    }

    fn restore_with(&self, output: &mut impl Write) -> Result<()> {
        let _output_lock = std::io::stdout().lock();
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
mod tests;
