use std::time::{Duration, Instant};

use ratatui::{Terminal, backend::Backend, layout::Position};

use super::{AppState, UiMode};

pub(super) struct CursorBlink {
    epoch: Instant,
    pub(super) visible: bool,
    pub(super) position: Option<Position>,
}

impl CursorBlink {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            epoch: now,
            visible: true,
            position: None,
        }
    }

    pub(super) fn show<B: Backend>(&self, terminal: &mut Terminal<B>) -> Result<(), B::Error> {
        let Some(position) = self.position else {
            return Ok(());
        };
        if self.visible {
            terminal.set_cursor_position(position)?;
            terminal.show_cursor()
        } else {
            terminal.hide_cursor()
        }
    }

    pub(super) fn update(&mut self, now: Instant, input: bool) -> bool {
        if input {
            self.epoch = now;
        }
        let visible = (now.duration_since(self.epoch).as_millis() / 500).is_multiple_of(2);
        let changed = visible != self.visible;
        self.visible = visible;
        changed
    }

    pub(super) fn timeout(&self, now: Instant) -> Duration {
        Duration::from_millis(500 - (now.duration_since(self.epoch).as_millis() % 500) as u64)
    }
}

impl AppState {
    pub(super) fn composer_cursor_enabled(&self) -> bool {
        self.mode == UiMode::Chat
            && !(self.error_open && self.last_error.is_some())
            && !self.has_local_script_approval()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redraws_do_not_restart_blink_but_input_does() {
        let start = Instant::now();
        let mut cursor = CursorBlink::new(start);
        for millis in 0..500 {
            assert!(!cursor.update(start + Duration::from_millis(millis), false));
            assert!(cursor.visible);
        }
        assert!(cursor.update(start + Duration::from_millis(500), false));
        assert!(!cursor.visible);
        assert!(!cursor.update(start + Duration::from_millis(700), false));
        assert_eq!(
            cursor.timeout(start + Duration::from_millis(700)),
            Duration::from_millis(300)
        );
        assert!(cursor.update(start + Duration::from_millis(750), true));
        assert!(cursor.visible);
        assert!(!cursor.update(start + Duration::from_millis(1000), false));
        assert!(cursor.update(start + Duration::from_millis(1250), false));
        assert!(!cursor.visible);
        assert!(cursor.update(start + Duration::from_millis(1750), false));
        assert!(cursor.visible);
    }

    #[test]
    #[expect(clippy::expect_used, reason = "the test terminal writes to memory")]
    fn blink_writes_only_cursor_commands() {
        use std::{cell::RefCell, io::Write, rc::Rc};

        use ratatui::{TerminalOptions, Viewport, backend::CrosstermBackend, layout::Rect};

        #[derive(Clone, Default)]
        struct Output(Rc<RefCell<Vec<u8>>>);
        impl Write for Output {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.borrow_mut().write(bytes)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let output = Output::default();
        let mut terminal = Terminal::with_options(
            CrosstermBackend::new(output.clone()),
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
            },
        )
        .expect("fixed viewport");
        let start = Instant::now();
        let mut cursor = CursorBlink::new(start);
        assert!(cursor.show(&mut terminal).is_ok());
        assert!(output.0.borrow().is_empty());

        cursor.position = Some(Position::new(4, 20));
        cursor.update(start + Duration::from_millis(500), false);
        assert!(cursor.show(&mut terminal).is_ok());
        assert_eq!(output.0.take(), b"\x1b[?25l");

        cursor.update(start + Duration::from_millis(1000), false);
        assert!(cursor.show(&mut terminal).is_ok());
        assert_eq!(output.0.take(), b"\x1b[21;5H\x1b[?25h");
    }
}
