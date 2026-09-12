use super::*;
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

#[test]
fn overflowing_approval_metadata_and_source_are_reachable() {
    let mut harness = test_harness();
    let mut script = pending_script("overflow");
    script.capability_additions = vec![String::from("workspace-write"); 20];
    script.requested_capabilities = vec![String::from("network")];
    script.source = String::from("echo source-tail");
    harness.app.state.pending_script = Some(script);
    harness.app.enter_script_decision_phase();
    for expanded in [false, true] {
        harness.app.state.approval_expanded = expanded;
        let mut seen_requested = false;
        let mut seen_timeout = false;
        let mut seen_source = false;
        for offset in 0..80 {
            harness.app.state.approval_scroll.set(offset);
            let area = Rect::new(0, 0, 24, 12);
            let mut buffer = Buffer::empty(area);
            (&harness.app.state).render(area, &mut buffer);
            let rendered = buffer
                .content()
                .chunks(24)
                .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            seen_requested |= rendered.contains("Requested: network");
            seen_timeout |= rendered.contains("Timeout:");
            seen_source |= rendered.contains("echo source-tail");
        }
        assert!(seen_requested);
        assert!(seen_timeout);
        assert!(seen_source);
    }
}
