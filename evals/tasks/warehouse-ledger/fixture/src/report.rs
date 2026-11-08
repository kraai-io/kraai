use std::fmt::Write;

use crate::ledger::State;

/// One `sku location quantity value` line per stocked pair, then the stock total and the
/// cost of everything shipped.
pub(crate) fn render(state: &State) -> String {
    let mut report = String::new();
    let mut total = 0;
    for (sku, location, quantity, value) in state.stock.rows() {
        let _ = writeln!(report, "{sku} {location} {quantity} {}", dollars(value));
        total += value;
    }
    let _ = writeln!(report, "total {}", dollars(total));
    let _ = writeln!(report, "shipped {}", dollars(state.shipped_cost));
    report
}

fn dollars(cents: u64) -> String {
    format!("{}.{}", cents / 100, cents % 100)
}
