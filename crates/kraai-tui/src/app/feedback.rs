use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CopyTarget {
    Error,
    Auth,
}

#[derive(Default)]
pub(super) struct VisualFeedback {
    pub(super) completion_until: Option<Instant>,
    copied: Option<(CopyTarget, Instant)>,
    last_fade_frame: Option<Instant>,
}

impl VisualFeedback {
    pub(super) fn completion_color(
        &self,
        now: Instant,
        muted: Option<ratatui::style::Color>,
    ) -> ratatui::style::Color {
        use ratatui::style::Color;
        let Some(until) = self.completion_until else {
            return Color::DarkGray;
        };
        let Some(Color::Rgb(r, g, b)) = muted else {
            return Color::DarkGray;
        };
        let remaining = until.saturating_duration_since(now).as_secs_f64().min(1.0);
        let blend = remaining * remaining * (3.0 - 2.0 * remaining);
        let channel = |start: u8, end: u8| {
            (f64::from(end) + (f64::from(start) - f64::from(end)) * blend).round() as u8
        };
        Color::Rgb(channel(174, r), channel(184, g), channel(210, b))
    }

    pub(super) fn advance_fade(&mut self, now: Instant) -> bool {
        if self.completion_until.is_none() {
            self.last_fade_frame = None;
            return false;
        }
        if self
            .last_fade_frame
            .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_millis(33))
        {
            return false;
        }
        self.last_fade_frame = Some(now);
        true
    }

    pub(super) fn copied(&self, target: CopyTarget) -> bool {
        self.copied.is_some_and(|(active, _)| active == target)
    }

    pub(super) fn show_copied(&mut self, target: CopyTarget, now: Instant) {
        self.copied = Some((target, now + Duration::from_millis(1200)));
    }

    pub(super) fn clear_copy(&mut self) {
        self.copied = None;
    }

    pub(super) fn expire(&mut self, now: Instant) -> bool {
        let completion = self.completion_until.is_some_and(|until| now >= until);
        let copied = self.copied.is_some_and(|(_, until)| now >= until);
        if completion {
            self.completion_until = None;
        }
        if copied {
            self.copied = None;
        }
        completion || copied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn completion_fades_to_the_terminal_palette_without_a_final_color_jump() {
        let now = Instant::now();
        let mut feedback = VisualFeedback {
            completion_until: Some(now + Duration::from_secs(1)),
            ..Default::default()
        };
        let muted = Some(Color::Rgb(88, 82, 115));
        assert_eq!(
            feedback.completion_color(now, muted),
            Color::Rgb(174, 184, 210)
        );
        assert_eq!(
            feedback.completion_color(now + Duration::from_millis(500), muted),
            Color::Rgb(131, 133, 163)
        );
        assert_eq!(
            feedback.completion_color(now + Duration::from_millis(999), muted),
            Color::Rgb(88, 82, 115)
        );
        assert_eq!(feedback.completion_color(now, None), Color::DarkGray);
        assert!(feedback.advance_fade(now));
        assert!(!feedback.advance_fade(now + Duration::from_millis(16)));
        assert!(feedback.advance_fade(now + Duration::from_millis(33)));
        assert!(feedback.expire(now + Duration::from_secs(1)));
        assert_eq!(
            feedback.completion_color(now + Duration::from_secs(1), muted),
            Color::DarkGray
        );
        assert!(!feedback.advance_fade(now + Duration::from_secs(1)));
    }
}
