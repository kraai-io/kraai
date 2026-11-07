use warehouse_ledger::{Ledger, LedgerError};

#[test]
fn receives_and_ships_one_lot() {
    let mut ledger = Ledger::new();
    ledger
        .apply("receive bolt 10 dock @250\nship bolt 4 dock\n")
        .unwrap();
    assert_eq!(ledger.quantity("bolt", "dock"), 6);
    assert_eq!(ledger.value_cents("bolt", "dock"), 1500);
    assert_eq!(ledger.shipped_cost_cents(), 1000);
    assert_eq!(
        ledger.report(),
        "bolt dock 6 15.00\ntotal 15.00\nshipped 10.00\n"
    );
}

#[test]
fn reports_unknown_commands_by_line() {
    let mut ledger = Ledger::new();
    assert_eq!(
        ledger.apply("receive bolt 1 dock @100\nrestock bolt 1 dock\n"),
        Err(LedgerError::Parse { line: 2 })
    );
}
