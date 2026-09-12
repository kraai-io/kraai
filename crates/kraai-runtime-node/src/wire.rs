use kraai_runtime::{RuntimeError, RuntimeResult};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

pub(crate) fn decode<T: DeserializeOwned>(mut value: Value) -> RuntimeResult<T> {
    normalize_numbers(&mut value).map_err(RuntimeError::invalid_argument)?;
    serde_json::from_value(value).map_err(|error| RuntimeError::invalid_argument(error.to_string()))
}

pub(crate) fn encode<T: Serialize>(result: RuntimeResult<T>) -> napi::Result<Value> {
    value(result)
}

pub(crate) fn value(value: impl Serialize) -> napi::Result<Value> {
    let mut value =
        serde_json::to_value(value).map_err(|error| napi::Error::from_reason(error.to_string()))?;
    normalize_numbers(&mut value).map_err(napi::Error::from_reason)?;
    Ok(value)
}

fn normalize_numbers(value: &mut Value) -> Result<(), String> {
    match value {
        Value::Number(number) => {
            if let Some(value) = number.as_f64() {
                if value.abs() > 9_007_199_254_740_991.0 {
                    return Err("number exceeds JavaScript's safe integer range".into());
                }
                if number.is_f64() && value.fract() == 0.0 {
                    *number = (value as i64).into();
                }
            }
            Ok(())
        }
        Value::Array(values) => values.iter_mut().try_for_each(normalize_numbers),
        Value::Object(values) => values.values_mut().try_for_each(normalize_numbers),
        _ => Ok(()),
    }
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "tests assert after fallible serialization"
)]
mod tests {
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
    fn preserves_validation_details() -> napi::Result<()> {
        let result = encode::<()>(Err(RuntimeError::validation(vec![
            kraai_runtime::FieldViolation {
                field: "models.0.id".into(),
                message: "required".into(),
            },
        ])))?;
        assert_eq!(
            result,
            json!({ "Err": {
                "kind": "validation", "message": "Settings validation failed",
                "violations": [{ "field": "models.0.id", "message": "required" }]
            }})
        );
        Ok(())
    }
}
