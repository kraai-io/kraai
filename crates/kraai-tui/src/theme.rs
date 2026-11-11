use std::time::Duration;

use ratatui::style::Color;

pub(crate) const ACCENT: Color = Color::Rgb(174, 184, 210);
pub(crate) const ON_ACCENT: Color = Color::Black;
pub(crate) const MUTED: Color = Color::DarkGray;
pub(crate) const RAISED: Color = Color::DarkGray;
pub(crate) const WAITING: Color = Color::Yellow;
pub(crate) const DANGER: Color = Color::Red;

/// How often a breathing colour takes a new shade.
pub(crate) const BREATH_FRAME: Duration = Duration::from_millis(50);
/// How long a breathing colour takes to dim.
const BREATH_FALL: Duration = Duration::from_millis(800);
/// How long a breathing colour takes to light up again.
const BREATH_RISE: Duration = Duration::from_millis(1600);
/// How much of its colour a breathing colour keeps at its dimmest.
const BREATH_FLOOR: f64 = 0.15;

/// `colour` `elapsed` into a breath that dims it toward `muted` and lights it up again, one
/// shade per `BREATH_FRAME`. Everything that breathes counts `elapsed` from the same moment,
/// so it all breathes together. `None` unless both colours are RGB.
pub(crate) fn breathe(elapsed: Duration, colour: Color, muted: Color) -> Option<Color> {
    let frame = BREATH_FRAME.as_millis();
    let phase = elapsed.as_millis() % (BREATH_FALL + BREATH_RISE).as_millis() / frame * frame;
    let share = |part: u128, whole: Duration| {
        let number = |millis: u128| f64::from(u32::try_from(millis).unwrap_or(u32::MAX));
        number(part) / number(whole.as_millis())
    };
    let glow = match phase.checked_sub(BREATH_FALL.as_millis()) {
        Some(rising) => ease(share(rising, BREATH_RISE)),
        None => 1.0 - ease(share(phase, BREATH_FALL)),
    };
    mix(muted, colour, BREATH_FLOOR + (1.0 - BREATH_FLOOR) * glow)
}

/// `amount` between 0 and 1, eased so a fade starts and stops without a jump.
pub(crate) fn ease(amount: f64) -> f64 {
    let amount = amount.clamp(0.0, 1.0);
    amount * amount * (3.0 - 2.0 * amount)
}

/// The colour `amount` of the way from `from` to `to`, or `None` unless both are RGB.
pub(crate) fn mix(from: Color, to: Color, amount: f64) -> Option<Color> {
    let (Color::Rgb(from_r, from_g, from_b), Color::Rgb(to_r, to_g, to_b)) = (from, to) else {
        return None;
    };
    let amount = amount.clamp(0.0, 1.0);
    let channel = |from: u8, to: u8| {
        (f64::from(from) + (f64::from(to) - f64::from(from)) * amount).round() as u8
    };
    Some(Color::Rgb(
        channel(from_r, to_r),
        channel(from_g, to_g),
        channel(from_b, to_b),
    ))
}

#[cfg(test)]
mod tests {
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
}
