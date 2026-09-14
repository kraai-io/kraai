use journal_recovery::{Delivery, Journal, JournalError, Operation};

fn put(id: u64, key: &str, value: &str) -> Delivery {
    Delivery {
        id,
        operation: Operation::Put {
            key: key.into(),
            value: value.into(),
        },
    }
}

#[test]
fn changes_survive_a_clean_restart() {
    let mut journal = Journal::open(&[]).unwrap();
    journal.append(put(8, "name", "raven")).unwrap();
    journal.append(put(2, "color", "black")).unwrap();
    journal
        .append(Delivery {
            id: 9,
            operation: Operation::Delete {
                key: "color".into(),
            },
        })
        .unwrap();
    let recovered = Journal::open(journal.bytes()).unwrap();
    assert_eq!(recovered.get("name"), Some("raven"));
    assert_eq!(recovered.get("color"), None);
    assert_eq!(recovered.snapshot(), journal.snapshot());
}

#[test]
fn live_duplicate_deliveries_write_nothing() {
    let mut journal = Journal::open(&[]).unwrap();
    let delivery = put(42, "key", "value");
    assert_eq!(journal.append(delivery.clone()), Ok(true));
    let before = journal.bytes().to_vec();
    assert_eq!(journal.append(delivery), Ok(false));
    assert_eq!(journal.bytes(), before);
    assert_eq!(
        journal.append(put(42, "key", "other")),
        Err(JournalError::Conflict(42))
    );
    assert_eq!(journal.bytes(), before);
    assert_eq!(journal.get("key"), Some("value"));
}
