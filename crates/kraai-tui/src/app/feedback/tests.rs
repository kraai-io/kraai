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
    assert_eq!(feedback.completion_color(now, muted), theme::ACCENT);
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

#[test]
fn timeout_wakes_for_fade_frames_and_expiry() {
    let now = Instant::now();
    let mut feedback = VisualFeedback::default();
    assert_eq!(feedback.timeout(now, true), None);

    feedback.show_copied(CopyTarget::Error, now);
    assert_eq!(
        feedback.timeout(now, true),
        Some(Duration::from_millis(1200))
    );
    assert_eq!(
        feedback.timeout(now + Duration::from_secs(2), false),
        Some(Duration::ZERO)
    );

    feedback.completion_until = Some(now + Duration::from_secs(1));
    assert_eq!(feedback.timeout(now, false), Some(Duration::from_secs(1)));
    assert_eq!(feedback.timeout(now, true), Some(FADE_FRAME));
}
