use transactional_recovery::{Check, Commit, Journal, Transaction, Write};

#[test]
fn commit_and_restart() {
    let mut journal = Journal::open(None, &[]).unwrap();
    let tx = Transaction {
        id: 10,
        checks: vec![Check {
            key: "x".into(),
            version: None,
        }],
        writes: vec![Write::Put {
            key: "x".into(),
            value: "hello".into(),
        }],
    };
    assert_eq!(
        journal.commit(tx.clone()).unwrap(),
        Commit {
            revision: 1,
            applied: true
        }
    );
    assert_eq!(journal.get("x"), Some("hello"));
    assert_eq!(journal.version("x"), Some(1));
    assert_eq!(
        Journal::open(None, journal.bytes()).unwrap().snapshot(),
        journal.snapshot()
    );
}

#[test]
fn checkpoint_starts_a_new_tail() {
    let mut journal = Journal::open(None, &[]).unwrap();
    journal
        .commit(Transaction {
            id: 1,
            checks: vec![],
            writes: vec![Write::Put {
                key: "a".into(),
                value: "b".into(),
            }],
        })
        .unwrap();
    let checkpoint = journal.checkpoint();
    let reopened = Journal::open(Some(&checkpoint), &[]).unwrap();
    assert_eq!(reopened.get("a"), Some("b"));
    assert!(reopened.bytes().is_empty());
}
