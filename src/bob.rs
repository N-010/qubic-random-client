use serde_json::Value;

pub(crate) fn extract_result(value: Value) -> Result<Value, String> {
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        return Err(match error.get("code").and_then(Value::as_i64) {
            Some(code) => format!("bob rpc error (code {code})"),
            None => "bob rpc error".to_string(),
        });
    }
    if let Some(result) = value.get("result") {
        return Ok(result.clone());
    }
    Ok(value)
}

pub(crate) fn value_to_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.parse::<u64>().ok(),
        _ => None,
    }
}

pub(crate) fn extract_u64_field(value: &Value, keys: &[&str]) -> Option<u64> {
    let Value::Object(map) = value else {
        return None;
    };
    keys.iter()
        .find_map(|key| map.get(*key))
        .and_then(value_to_u64)
}

pub(crate) fn extract_string_field(value: &Value, keys: &[&str]) -> Option<String> {
    let Value::Object(map) = value else {
        return None;
    };
    keys.iter()
        .find_map(|key| map.get(*key).and_then(Value::as_str))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rpc_errors_expose_only_numeric_codes() {
        for error in [
            json!({"code": -32000, "message": "credential=example-secret", "data": "echoed request"}),
            json!({"code": "example-secret", "message": "echoed request"}),
            json!("credential=example-secret"),
        ] {
            let expected = if error.get("code").and_then(Value::as_i64).is_some() {
                "bob rpc error (code -32000)"
            } else {
                "bob rpc error"
            };
            assert_eq!(
                extract_result(json!({"error": error})),
                Err(expected.into())
            );
        }
    }

    #[test]
    fn transaction_id_aliases_skip_non_string_fields() {
        let keys = ["transactionId", "txId", "hash", "id"];
        assert_eq!(
            extract_string_field(
                &json!({"transactionId": null, "txId": 7, "hash": "accepted"}),
                &keys
            ),
            Some("accepted".to_string()),
        );
        assert_eq!(
            extract_string_field(
                &json!({"transactionId": "preferred", "hash": "other"}),
                &keys
            ),
            Some("preferred".to_string()),
        );
        assert_eq!(
            extract_string_field(&json!({"transactionId": null, "id": 7}), &keys),
            None
        );
    }

    #[test]
    fn null_error_does_not_hide_a_successful_result() {
        let result = json!({"currentVerifyLoggingTick": 123});
        assert_eq!(
            extract_result(json!({"result": result.clone(), "error": null})),
            Ok(result),
        );
    }

    #[test]
    fn non_null_error_takes_precedence_over_result() {
        assert!(
            extract_result(json!({
                "result": {"transfers": []},
                "error": {"code": -32000, "message": "unavailable"},
            }))
            .is_err()
        );
    }
}
