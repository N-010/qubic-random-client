use serde_json::Value;

pub(crate) fn extract_result(value: Value) -> Result<Value, String> {
    if let Some(error) = value.get("error") {
        return Err(format!("bob rpc error: {}", error));
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
        .find_map(|key| map.get(*key))
        .and_then(|value| value.as_str().map(str::to_string))
}
