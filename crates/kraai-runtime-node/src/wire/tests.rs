use super::*;
use serde_json::json;

#[test]
fn rejects_lossy_numbers_and_invalid_arguments() {
    assert!(decode::<usize>(json!(-1)).is_err());
    assert!(decode::<usize>(json!(1.5)).is_err());
    assert!(decode::<usize>(json!(9_007_199_254_740_992_u64)).is_err());
    assert!(value(json!({ "sequence": u64::MAX })).is_err());
    assert!(decode::<u64>(json!(9_007_199_254_740_991_u64)).is_ok());
    assert_eq!(decode::<u64>(json!(4_294_967_296.0)), Ok(4_294_967_296));
    assert_eq!(decode::<i64>(json!(-4_294_967_296.0)), Ok(-4_294_967_296));
}

#[test]
fn preserves_validation_details() {
    let result = encode::<()>(Err(RuntimeError::validation(vec![
        kraai_runtime::FieldViolation {
            field: "models.0.id".into(),
            message: "required".into(),
        },
    ])))
    .unwrap();
    assert_eq!(
        result,
        json!({ "Err": {
            "kind": "validation", "message": "Settings validation failed",
            "violations": [{ "field": "models.0.id", "message": "required" }]
        }})
    );
}
