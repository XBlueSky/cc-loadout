#![allow(dead_code)]
use anyhow::Result;
use serde::Serialize;

/// Schema version of the CLI's `--json` contract. Additive changes (new keys) do
/// NOT bump this; consumers MUST tolerate unknown keys. Breaking changes (rename /
/// remove / retype an existing key) bump it.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize)]
struct Envelope<'a, T: Serialize> {
    schema_version: u32,
    #[serde(flatten)]
    payload: &'a T,
}

/// Serialize `payload` inside the versioned envelope as pretty JSON.
pub fn to_string<T: Serialize>(payload: &T) -> Result<String> {
    let env = Envelope {
        schema_version: SCHEMA_VERSION,
        payload,
    };
    Ok(serde_json::to_string_pretty(&env)?)
}

/// Print the enveloped payload to stdout.
pub fn emit<T: Serialize>(payload: &T) -> Result<()> {
    println!("{}", to_string(payload)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn envelope_flattens_payload_with_schema_version() {
        let s = to_string(&json!({ "accounts": [] })).unwrap();
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["schema_version"], json!(1));
        assert_eq!(v["accounts"], json!([]));
        // schema_version sits flat alongside payload keys, not nested under "data".
        assert!(v.get("data").is_none());
    }
}
