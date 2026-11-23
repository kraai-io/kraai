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

#[derive(Default)]
pub(super) struct Note {
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
    pub(super) choices: Vec<String>,
    pub(super) chosen: Option<usize>,
    pub(super) detail: String,
    pub(super) meta: String,
    pub(super) tag: String,
    pub(super) waiting: bool,
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
    offset: usize,
    text: String,
    tone: Tone,
}

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
                tone @ (Tone::Waiting | Tone::Danger) => tone.over(base),
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
            tone @ (Tone::Plain | Tone::Danger) => tone.over(base),
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
mod tests;
