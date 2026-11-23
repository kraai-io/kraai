use std::time::Duration;

use ratatui::style::Color;

pub(crate) const ACCENT: Color = Color::Rgb(174, 184, 210);
pub(crate) const ON_ACCENT: Color = Color::Black;
pub(crate) const MUTED: Color = Color::DarkGray;
pub(crate) const RAISED: Color = Color::DarkGray;
pub(crate) const WAITING: Color = Color::Yellow;
pub(crate) const DANGER: Color = Color::Red;

pub(crate) const BREATH_FRAME: Duration = Duration::from_millis(50);
const BREATH_FALL: Duration = Duration::from_millis(800);
const BREATH_RISE: Duration = Duration::from_millis(1600);
const BREATH_FLOOR: f64 = 0.15;

/// Everything that breathes counts `elapsed` from the same moment, so it all breathes together.
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

pub(crate) fn ease(amount: f64) -> f64 {
    let amount = amount.clamp(0.0, 1.0);
    amount * amount * (3.0 - 2.0 * amount)
}

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
mod tests;
