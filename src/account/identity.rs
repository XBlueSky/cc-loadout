use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Stable identity of a Claude account: email plus optional organization.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Identity {
    pub email: String,
    pub org_uuid: Option<String>,
    pub org_name: Option<String>,
}

/// Extract identity fields from an `oauthAccount` JSON object.
pub fn extract(oauth_account: &Value) -> Identity {
    let s = |k: &str| {
        oauth_account
            .get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    Identity {
        email: s("emailAddress").unwrap_or_default(),
        org_uuid: s("organizationUuid"),
        org_name: s("organizationName"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_email_org() {
        let oauth = json!({
            "emailAddress": "a@b.com",
            "organizationUuid": "org-1",
            "organizationName": "Acme"
        });
        let id = extract(&oauth);
        assert_eq!(id.email, "a@b.com");
        assert_eq!(id.org_uuid.as_deref(), Some("org-1"));
        assert_eq!(id.org_name.as_deref(), Some("Acme"));
    }

    #[test]
    fn missing_fields_default() {
        let oauth = json!({ "emailAddress": "a@b.com" });
        let id = extract(&oauth);
        assert_eq!(id.email, "a@b.com");
        assert!(id.org_uuid.is_none());
        assert!(id.org_name.is_none());
    }
}
