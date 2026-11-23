use super::*;

#[test]
fn ease_is_flat_at_both_ends_and_even_in_the_middle() {
    for (amount, eased) in [
        (-1.0, 0.0),
        (0.0, 0.0),
        (0.25, 0.15625),
        (0.5, 0.5),
        (0.75, 0.84375),
        (1.0, 1.0),
        (2.0, 1.0),
    ] {
        assert!((ease(amount) - eased).abs() < 1e-12, "{amount}");
    }
}

#[test]
fn mix_moves_each_channel_and_needs_two_rgb_colours() {
    let from = Color::Rgb(88, 82, 115);
    assert_eq!(mix(from, ACCENT, 0.0), Some(from));
    assert_eq!(mix(from, ACCENT, 0.5), Some(Color::Rgb(131, 133, 163)));
    assert_eq!(mix(from, ACCENT, 1.0), Some(ACCENT));
    assert_eq!(mix(from, ACCENT, 7.0), Some(ACCENT));
    assert_eq!(mix(ACCENT, from, 1.0), Some(from));
    assert_eq!(mix(MUTED, ACCENT, 0.5), None);
    assert_eq!(mix(ACCENT, MUTED, 0.5), None);
}
