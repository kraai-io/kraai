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
