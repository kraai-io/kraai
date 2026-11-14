use std::{cell::Cell, ops::Range};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    widgets::{Clear, Widget},
};

use super::status::fit;
use crate::components::{display_width, normalize_terminal_text, wrap_words};
use crate::theme;

const INDENT: usize = 2;
const GAP: usize = 2;
const MAX_NOTE_ROWS: u16 = 8;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Tone {
    #[default]
    Plain,
    Waiting,
    Danger,
}

impl Tone {
    fn over(self, base: Style) -> Style {
        match self {
            Self::Plain => base,
            Self::Waiting => base.fg(theme::WAITING),
            Self::Danger => base.fg(theme::DANGER),
        }
    }
}

/// A line of text under the rows that stays in view while the rows scroll.
#[derive(Default)]
pub(super) struct Note {
    /// Names the text, in a column every note of the sheet shares.
    pub(super) label: String,
    pub(super) text: String,
    pub(super) tone: Tone,
}

impl Note {
    pub(super) fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            label: String::new(),
            text: text.into(),
            tone,
        }
    }

    pub(super) fn field(label: &str, text: impl Into<String>) -> Self {
        Self {
            label: label.to_owned(),
            text: text.into(),
            tone: Tone::Plain,
        }
    }
}

#[derive(Default)]
pub(super) struct SheetItem {
    pub(super) label: String,
    pub(super) value: String,
    /// Drawn in the value column on one line, with the chosen one boxed.
    pub(super) choices: Vec<String>,
    pub(super) chosen: Option<usize>,
    pub(super) detail: String,
    pub(super) meta: String,
    pub(super) tag: String,
    pub(super) waiting: bool,
    /// Colours the label and the tag. A plain row keeps its tag in the accent.
    pub(super) tone: Tone,
}

pub(super) enum SheetRow {
    Heading(String),
    Item(SheetItem),
}

pub(super) struct Sheet {
    pub(super) caption: String,
    pub(super) count: String,
    pub(super) rows: Vec<SheetRow>,
    pub(super) selected: usize,
    pub(super) empty: String,
    pub(super) notes: Vec<Note>,
    /// Choices share one column wide enough for the longest row. When false, each row's detail
    /// follows straight after its own choices.
    pub(super) align_choices: bool,
}

pub(super) fn render(
    sheet: &Sheet,
    scroll: &Cell<usize>,
    chat: Rect,
    screen: Rect,
    buf: &mut Buffer,
) {
    let width = usize::from(chat.width);
    let text_width = width.saturating_sub(INDENT + 1);
    let notes = note_lines(&sheet.notes, text_width);
    let limit = (screen.height / 2).max(3).min(chat.height);
    let note_rows = u16::try_from(notes.len())
        .unwrap_or(u16::MAX)
        .min(MAX_NOTE_ROWS)
        .min(limit.saturating_sub(2));
    let wanted = u16::try_from(sheet.rows.len().max(1))
        .unwrap_or(u16::MAX)
        .saturating_add(1 + note_rows);
    let height = wanted.min(limit);
    if height == 0 || chat.width == 0 {
        return;
    }
    let area = Rect::new(chat.x, chat.bottom() - height, chat.width, height);
    Clear.render(area, buf);
    buf.set_string(
        area.x,
        area.y,
        rule(&sheet.caption, &sheet.count, width),
        Style::default().fg(theme::MUTED),
    );

    let mut body = Rect::new(area.x, area.y + 1, area.width, height - 1);
    body.height = body.height.saturating_sub(note_rows);
    for (line, note) in (0..note_rows).zip(&notes) {
        let y = body.bottom() + line;
        put(
            buf,
            body.x,
            y,
            INDENT,
            &note.label,
            Style::default().fg(theme::MUTED),
        );
        put(
            buf,
            body.x,
            y,
            INDENT + note.offset,
            &fit(&note.text, text_width.saturating_sub(note.offset)),
            note.tone.over(Style::default()),
        );
    }
    if sheet.rows.is_empty() {
        if body.height > 0 {
            put(
                buf,
                body.x,
                body.y,
                INDENT,
                &fit(&sheet.empty, text_width),
                Style::default().fg(theme::MUTED),
            );
        }
        return;
    }

    let selected_row = selected_row(&sheet.rows, sheet.selected);
    let capacity = usize::from(body.height);
    let total = sheet.rows.len();
    let offset = window(&sheet.rows, selected_row, capacity, scroll.get());
    scroll.set(offset);
    let markers = capacity >= 3;
    let columns = Columns::measure(sheet, width);
    for line in 0..capacity {
        let index = offset + line;
        let Some(row) = sheet.rows.get(index) else {
            break;
        };
        let y = body.y + line as u16;
        let hidden = if markers && line == 0 && offset > 0 {
            Some(('▴', sheet.rows.get(..=index)))
        } else if markers && line + 1 == capacity && index + 1 < total {
            Some(('▾', sheet.rows.get(index..)))
        } else {
            None
        };
        if let Some((arrow, rows)) = hidden {
            let count = rows.map_or(0, items);
            put(
                buf,
                body.x,
                y,
                INDENT,
                &format!("{arrow} {count} more"),
                Style::default().fg(theme::MUTED),
            );
            continue;
        }
        let line_area = Rect::new(body.x, y, body.width, 1);
        match row {
            SheetRow::Heading(text) => put(
                buf,
                body.x,
                y,
                INDENT,
                &fit(text, text_width),
                Style::default().fg(theme::MUTED),
            ),
            SheetRow::Item(item) => {
                render_item(item, selected_row == Some(index), &columns, line_area, buf)
            }
        }
    }
}

struct NoteLine {
    label: String,
    /// The column the text starts in. Notes with a label share one; the others start at zero.
    offset: usize,
    text: String,
    tone: Tone,
}

/// Wraps the notes to `width`, one entry per line.
fn note_lines(notes: &[Note], width: usize) -> Vec<NoteLine> {
    let label = notes
        .iter()
        .map(|note| display_width(&note.label))
        .max()
        .unwrap_or(0)
        .min(width / 3);
    let mut lines = Vec::new();
    for note in notes {
        let offset = if note.label.is_empty() {
            0
        } else {
            label + GAP
        };
        let mut wrapped = wrap_words(
            &normalize_terminal_text(&note.text),
            width.saturating_sub(offset),
        );
        if wrapped.is_empty() {
            wrapped.push(String::new());
        }
        for (index, text) in wrapped.into_iter().enumerate() {
            lines.push(NoteLine {
                label: if index == 0 {
                    fit(&note.label, label)
                } else {
                    String::new()
                },
                offset,
                text,
                tone: note.tone,
            });
        }
    }
    lines
}

fn rule(caption: &str, count: &str, width: usize) -> String {
    let mut right = if count.is_empty() {
        String::new()
    } else {
        format!(" {count} ─")
    };
    let mut left = if caption.is_empty() {
        String::new()
    } else {
        format!("─ {caption} ")
    };
    if display_width(&left) + display_width(&right) + 1 > width {
        right.clear();
    }
    if display_width(&left) + 1 > width {
        left.clear();
    }
    let fill = width.saturating_sub(display_width(&left) + display_width(&right));
    format!("{left}{}{right}", "─".repeat(fill))
}

fn items(rows: &[SheetRow]) -> usize {
    rows.iter()
        .filter(|row| matches!(row, SheetRow::Item(_)))
        .count()
}

fn selected_row(rows: &[SheetRow], selected: usize) -> Option<usize> {
    let selected = selected.min(items(rows).checked_sub(1)?);
    rows.iter()
        .enumerate()
        .filter(|(_, row)| matches!(row, SheetRow::Item(_)))
        .nth(selected)
        .map(|(index, _)| index)
}

/// Returns the first visible row, moving the previous window only as far as the selection needs.
fn window(rows: &[SheetRow], selected: Option<usize>, capacity: usize, scroll: usize) -> usize {
    let total = rows.len();
    if capacity == 0 || total <= capacity {
        return 0;
    }
    let max_scroll = total - capacity;
    let scroll = scroll.min(max_scroll);
    let Some(selected) = selected else {
        return scroll;
    };
    if capacity < 3 {
        return scroll
            .min(selected)
            .max(selected.saturating_sub(capacity - 1));
    }
    let focus_top = selected
        .checked_sub(1)
        .filter(|above| matches!(rows.get(*above), Some(SheetRow::Heading(_))))
        .unwrap_or(selected);
    let first = scroll + usize::from(scroll > 0);
    let last_shown = |scroll: usize| scroll + capacity - 1 - usize::from(scroll < max_scroll);
    let last = last_shown(scroll);
    if focus_top < first {
        let with_heading = focus_top.saturating_sub(1);
        if selected <= last_shown(with_heading) {
            with_heading
        } else {
            selected.saturating_sub(1).min(max_scroll)
        }
    } else if selected > last {
        (selected + 2 - capacity).min(max_scroll)
    } else {
        scroll
    }
}

struct Columns {
    label: usize,
    value: usize,
}

impl Columns {
    fn measure(sheet: &Sheet, width: usize) -> Self {
        let mut columns = Self { label: 0, value: 0 };
        for row in &sheet.rows {
            if let SheetRow::Item(item) = row {
                if item.value.is_empty() && item.choices.is_empty() && item.detail.is_empty() {
                    continue;
                }
                columns.label = columns.label.max(display_width(&item.label));
                columns.value = columns.value.max(display_width(&item.value));
                if sheet.align_choices {
                    columns.value = columns.value.max(choices_width(&item.choices));
                }
            }
        }
        columns.label = columns.label.min(width / 2);
        columns.value = columns
            .value
            .min(width.saturating_sub(INDENT + columns.label + GAP + 1));
        columns
    }
}

fn choices_width(choices: &[String]) -> usize {
    choices
        .iter()
        .map(|choice| display_width(choice))
        .sum::<usize>()
        + GAP * choices.len().saturating_sub(1)
}

/// The run of choices that fits in `room` columns, grown around the chosen one. A cut end costs
/// a gap and one column for its `…`.
fn choice_run(choices: &[String], chosen: Option<usize>, room: usize) -> Range<usize> {
    let len = choices.len();
    let cost = |run: &Range<usize>| {
        choices_width(choices.get(run.clone()).unwrap_or_default())
            + if run.start > 0 { 1 + GAP } else { 0 }
            + if run.end < len { GAP + 1 } else { 0 }
    };
    let chosen = chosen.unwrap_or(0).min(len.saturating_sub(1));
    let mut run = chosen..(chosen + 1).min(len);
    loop {
        let left = run.start.checked_sub(1).map(|start| start..run.end);
        let right = (run.end < len).then(|| run.start..run.end + 1);
        let Some(wider) = [left, right]
            .into_iter()
            .flatten()
            .find(|wider| cost(wider) <= room)
        else {
            return run;
        };
        run = wider;
    }
}

struct ChoiceStyles {
    chosen: Style,
    other: Style,
    mark: Style,
}

/// Draws the choices from column `x` and returns how many columns they took.
fn render_choices(
    item: &SheetItem,
    styles: &ChoiceStyles,
    start: usize,
    end: usize,
    area: Rect,
    buf: &mut Buffer,
) -> usize {
    let mut x = start;
    let run = choice_run(&item.choices, item.chosen, end.saturating_sub(x));
    if run.start > 0 {
        put(buf, area.x, area.y, x, "…", styles.mark);
        x += 1 + GAP;
    }
    for index in run.clone() {
        let Some(choice) = item.choices.get(index) else {
            break;
        };
        let text = fit(choice, end.saturating_sub(x));
        if item.chosen == Some(index) {
            let boxed = format!(" {text} ");
            put(
                buf,
                area.x,
                area.y,
                x.saturating_sub(1),
                &boxed,
                styles.chosen,
            );
        } else {
            put(buf, area.x, area.y, x, &text, styles.other);
        }
        x += display_width(&text) + GAP;
    }
    if run.end < item.choices.len() {
        put(buf, area.x, area.y, x, "…", styles.mark);
        x += 1 + GAP;
    }
    x.saturating_sub(start + GAP)
}

fn render_item(item: &SheetItem, selected: bool, columns: &Columns, area: Rect, buf: &mut Buffer) {
    let width = usize::from(area.width);
    let base = if selected {
        Style::default().bg(theme::RAISED)
    } else {
        Style::default()
    };
    let muted = if selected {
        base
    } else {
        base.fg(theme::MUTED)
    };
    buf.set_style(area, base);
    if selected {
        buf.set_string(area.x, area.y, "▌", base.fg(theme::ACCENT));
    }

    let mut end = width.saturating_sub(1);
    let right_width = display_width(&item.tag)
        + display_width(&item.meta)
        + if item.tag.is_empty() || item.meta.is_empty() {
            0
        } else {
            GAP
        };
    if right_width > 0 && INDENT + columns.label + GAP + right_width <= end {
        let mut x = end - right_width;
        end = x.saturating_sub(GAP);
        if !item.tag.is_empty() {
            let tag = match item.tone {
                Tone::Plain => base.fg(theme::ACCENT),
                tone => tone.over(base),
            };
            put(buf, area.x, area.y, x, &item.tag, tag);
            x += display_width(&item.tag) + GAP;
        }
        put(buf, area.x, area.y, x, &item.meta, muted);
    }

    let mut x = INDENT;
    put(
        buf,
        area.x,
        area.y,
        x,
        &fit(&item.label, end.saturating_sub(x)),
        match item.tone {
            Tone::Waiting => base,
            tone => tone.over(base),
        },
    );
    x += columns.label + GAP;
    let waiting = base.fg(theme::WAITING);
    let value_width = if item.choices.is_empty() {
        put(
            buf,
            area.x,
            area.y,
            x,
            &fit(&item.value, end.saturating_sub(x)),
            if item.waiting { waiting } else { base },
        );
        columns.value
    } else {
        let styles = ChoiceStyles {
            chosen: if selected {
                Style::default().bg(theme::ACCENT).fg(theme::ON_ACCENT)
            } else {
                Style::default().bg(theme::RAISED)
            },
            other: if item.waiting { waiting } else { muted },
            mark: muted,
        };
        columns
            .value
            .max(render_choices(item, &styles, x, end, area, buf))
    };
    if value_width > 0 {
        x += value_width + GAP;
    }
    put(
        buf,
        area.x,
        area.y,
        x,
        &fit(&item.detail, end.saturating_sub(x)),
        muted,
    );
}

fn put(buf: &mut Buffer, left: u16, y: u16, column: usize, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    let Ok(column) = u16::try_from(column) else {
        return;
    };
    let x = left.saturating_add(column);
    if x < buf.area.right() && y < buf.area.bottom() {
        buf.set_string(x, y, text, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str) -> SheetRow {
        SheetRow::Item(SheetItem {
            label: label.to_owned(),
            ..SheetItem::default()
        })
    }

    fn sheet(rows: Vec<SheetRow>, selected: usize) -> Sheet {
        Sheet {
            caption: String::from("model"),
            count: String::from("9 models"),
            rows,
            selected,
            empty: String::from("Nothing here"),
            notes: Vec::new(),
            align_choices: true,
        }
    }

    fn lines(sheet: &Sheet, scroll: &Cell<usize>, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render(sheet, scroll, area, area, &mut buffer);
        buffer
            .content()
            .chunks(usize::from(width))
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn sheet_sits_at_the_bottom_under_a_rule_with_caption_and_count() {
        let rows = vec![SheetRow::Heading(String::from("anthropic")), item("Opus")];
        let lines = lines(&sheet(rows, 0), &Cell::new(0), 30, 10);
        assert_eq!(lines.get(6).map(String::as_str), Some(""));
        assert_eq!(
            lines.get(7).cloned(),
            Some(format!("─ model {} 9 models ─", "─".repeat(11)))
        );
        assert_eq!(lines.get(8).map(String::as_str), Some("  anthropic"));
        assert_eq!(lines.get(9).map(String::as_str), Some("▌ Opus"));
    }

    #[test]
    fn columns_align_and_right_side_holds_tag_and_meta() {
        let rows = vec![
            SheetRow::Item(SheetItem {
                label: String::from("Opus"),
                detail: String::from("high"),
                meta: String::from("1M"),
                tag: String::from("current"),
                ..SheetItem::default()
            }),
            SheetRow::Item(SheetItem {
                label: String::from("Sonnet 5"),
                detail: String::from("a detail far too long to fit in this row"),
                meta: String::from("200k"),
                ..SheetItem::default()
            }),
        ];
        let lines = lines(&sheet(rows, 1), &Cell::new(0), 36, 6);
        assert_eq!(
            lines.get(4).map(String::as_str),
            Some("  Opus      high        current  1M")
        );
        assert_eq!(
            lines.get(5).map(String::as_str),
            Some("▌ Sonnet 5  a detail far too…  200k")
        );
    }

    #[test]
    fn selected_row_is_raised_and_empty_and_notice_lines_render() {
        let area = Rect::new(0, 0, 20, 8);
        let mut buffer = Buffer::empty(area);
        render(
            &sheet(vec![item("one"), item("two")], 1),
            &Cell::new(0),
            area,
            area,
            &mut buffer,
        );
        assert_eq!(buffer[(0, 7)].fg, theme::ACCENT);
        assert_eq!(buffer[(19, 7)].bg, theme::RAISED);
        assert_ne!(buffer[(19, 6)].bg, theme::RAISED);

        let mut empty = sheet(Vec::new(), 0);
        empty.notes = vec![Note::new("Budget is out of range", Tone::Danger)];
        let lines = lines(&empty, &Cell::new(0), 30, 8);
        assert_eq!(lines.get(6).map(String::as_str), Some("  Nothing here"));
        assert_eq!(
            lines.get(7).map(String::as_str),
            Some("  Budget is out of range")
        );
    }

    #[test]
    fn long_notice_wraps_and_leaves_room_for_a_row() {
        let mut view = sheet(vec![item("one"), item("two"), item("three")], 2);
        view.notes = vec![Note::new(
            "budget: Budget is outside its supported bounds",
            Tone::Danger,
        )];
        let lines = lines(&view, &Cell::new(0), 30, 8);
        assert_eq!(
            lines.get(4..).unwrap_or_default(),
            [
                format!("─ model {} 9 models ─", "─".repeat(11)),
                String::from("▌ three"),
                String::from("  budget: Budget is outside"),
                String::from("  its supported bounds"),
            ]
        );
    }

    fn choice_item(label: &str, choices: &[&str], chosen: Option<usize>) -> SheetRow {
        SheetRow::Item(SheetItem {
            label: label.to_owned(),
            choices: choices.iter().map(|choice| (*choice).to_owned()).collect(),
            chosen,
            ..SheetItem::default()
        })
    }

    #[test]
    fn choices_share_a_line_and_the_chosen_one_is_boxed() {
        let rows = vec![
            choice_item("Effort", &["Low", "Medium", "High"], Some(1)),
            choice_item("Thinking", &["off", "on"], Some(1)),
            SheetRow::Item(SheetItem {
                label: String::from("Budget"),
                value: String::from("50"),
                meta: String::from("10 to 100"),
                ..SheetItem::default()
            }),
        ];
        let area = Rect::new(0, 0, 40, 8);
        let mut buffer = Buffer::empty(area);
        render(&sheet(rows, 0), &Cell::new(0), area, area, &mut buffer);
        let text: Vec<String> = buffer
            .content()
            .chunks(40)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect();
        assert_eq!(
            text.get(5..).unwrap_or_default(),
            [
                format!("{:<40}", "▌ Effort    Low  Medium  High"),
                format!("{:<40}", "  Thinking  off  on"),
                format!("{:<40}", "  Budget    50                10 to 100"),
            ]
        );
        for x in 16..=23 {
            assert_eq!(buffer[(x, 5)].bg, theme::ACCENT, "column {x}");
            assert_eq!(buffer[(x, 5)].fg, theme::ON_ACCENT, "column {x}");
        }
        for x in [15, 24] {
            assert_eq!(buffer[(x, 5)].bg, theme::RAISED, "column {x}");
        }
        for x in 16..=19 {
            assert_eq!(buffer[(x, 6)].bg, theme::RAISED, "column {x}");
        }
        assert_ne!(buffer[(15, 6)].bg, theme::RAISED);
        assert_eq!(buffer[(12, 6)].fg, theme::MUTED);
    }

    #[test]
    fn choices_that_do_not_fit_keep_the_chosen_one_in_view() {
        let choices = ["None", "Minimal", "Low", "Medium", "High", "Maximum"];
        let line = |chosen| {
            let rows = vec![choice_item("Effort", &choices, chosen)];
            lines(&sheet(rows, 0), &Cell::new(0), 34, 2)
                .pop()
                .unwrap_or_default()
        };
        assert_eq!(line(Some(0)), "▌ Effort  None  Minimal  Low  …");
        assert_eq!(line(Some(3)), "▌ Effort  …  Low  Medium  High  …");
        assert_eq!(line(Some(5)), "▌ Effort  …  High  Maximum");
        assert_eq!(line(None), "▌ Effort  None  Minimal  Low  …");
    }

    #[test]
    fn long_lists_scroll_only_as_far_as_the_selection_needs() {
        let rows: Vec<_> = (0..12).map(|index| item(&format!("row {index}"))).collect();
        let scroll = Cell::new(0);
        let mut view = sheet(rows, 0);
        let first = lines(&view, &scroll, 20, 12);
        assert_eq!(first.get(7).map(String::as_str), Some("▌ row 0"));
        assert_eq!(first.get(11).map(String::as_str), Some("  ▾ 8 more"));

        view.selected = 11;
        let last = lines(&view, &scroll, 20, 12);
        assert_eq!(last.get(7).map(String::as_str), Some("  ▴ 8 more"));
        assert_eq!(last.get(11).map(String::as_str), Some("▌ row 11"));
        assert_eq!(scroll.get(), 7);

        view.selected = 10;
        lines(&view, &scroll, 20, 12);
        assert_eq!(scroll.get(), 7);

        view.selected = 7;
        let up = lines(&view, &scroll, 20, 12);
        assert_eq!(scroll.get(), 6);
        assert_eq!(up.get(7).map(String::as_str), Some("  ▴ 7 more"));
        assert_eq!(up.get(8).map(String::as_str), Some("▌ row 7"));
        assert_eq!(up.get(11).map(String::as_str), Some("  ▾ 2 more"));
    }

    #[test]
    fn scrolling_up_keeps_the_heading_above_the_selection_visible() {
        let mut rows = vec![SheetRow::Heading(String::from("a")), item("a1")];
        rows.push(SheetRow::Heading(String::from("b")));
        rows.extend((0..8).map(|index| item(&format!("b{index}"))));
        assert_eq!(window(&rows, Some(3), 4, 6), 1);
        assert_eq!(window(&rows, Some(1), 4, 6), 0);
        assert_eq!(window(&rows, Some(10), 4, 0), 7);
        assert_eq!(window(&rows, Some(5), 2, 0), 4);
        assert_eq!(window(&rows, None, 4, 99), 7);
    }

    #[test]
    fn three_rows_keep_the_selection_when_its_heading_does_not_fit_with_it() {
        let mut rows = vec![SheetRow::Heading(String::from("a")), item("a1")];
        rows.push(SheetRow::Heading(String::from("b")));
        rows.extend((0..4).map(|index| item(&format!("b{index}"))));
        assert_eq!(window(&rows, Some(3), 3, 4), 2);
        assert_eq!(window(&rows, Some(3), 3, 2), 2);
    }
}
