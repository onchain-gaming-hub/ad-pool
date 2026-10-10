//! `click_url` layer 2 (A14 · W2): Google **Web Risk Lookup API** `uris:search` — one call
//! per URL (submitted URL + every redirect hop). Never `computeDiff` (plan W2).
//! The API key goes in the `x-goog-api-key` header, so no URL ever carries a secret
//! (verified 2026-10-10: header key is accepted — the project answered `BILLING_DISABLED`,
//! a bad key answers `API_KEY_INVALID`).
//! ⚠️ Response shapes (`{}` = no match · `{"threat":{"threatTypes":[…],"expireTime":…}}`) are
//! from the API reference — NOT yet seen live: the GCP project has billing disabled.
//! Every failure is `Unavailable` → the Worker never approves without a layer-2 answer.

use serde_json::Value;

pub const SEARCH_URL: &str = "https://webrisk.googleapis.com/v1/uris:search";
pub const THREAT_TYPES: [&str; 3] = ["MALWARE", "SOCIAL_ENGINEERING", "UNWANTED_SOFTWARE"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskError {
    Http(u16),
    /// 403 `BILLING_DISABLED` — needs the project owner, retrying will not help
    BillingDisabled,
    Transport,
    Timeout,
    BadResponse,
    MissingKey,
}

impl RiskError {
    pub fn code(&self) -> String {
        match self {
            RiskError::Http(s) => format!("http_{s}"),
            RiskError::BillingDisabled => "billing_disabled".into(),
            RiskError::Transport => "transport".into(),
            RiskError::Timeout => "timeout".into(),
            RiskError::BadResponse => "bad_response".into(),
            RiskError::MissingKey => "missing_key".into(),
        }
    }
}

/// Request URL (no key — that is a header).
pub fn search_url(uri: &str) -> String {
    let mut u = url::Url::parse(SEARCH_URL).expect("constant URL");
    {
        let mut q = u.query_pairs_mut();
        for t in THREAT_TYPES {
            q.append_pair("threatTypes", t);
        }
        q.append_pair("uri", uri);
    }
    u.into()
}

/// `Ok(vec![])` = no match; otherwise the matched threat types.
pub fn parse(status: u16, body: &str) -> Result<Vec<String>, RiskError> {
    if status == 403 && body.contains("BILLING_DISABLED") {
        return Err(RiskError::BillingDisabled);
    }
    if status != 200 {
        return Err(RiskError::Http(status));
    }
    let v: Value = serde_json::from_str(body).map_err(|_| RiskError::BadResponse)?;
    let obj = v.as_object().ok_or(RiskError::BadResponse)?;
    let Some(threat) = obj.get("threat") else {
        // an empty object is the documented "no match"; anything else is unexpected
        return if obj.is_empty() {
            Ok(vec![])
        } else {
            Err(RiskError::BadResponse)
        };
    };
    let types: Vec<String> = threat
        .get("threatTypes")
        .and_then(Value::as_array)
        .ok_or(RiskError::BadResponse)?
        .iter()
        .filter_map(|t| t.as_str().map(str::to_string))
        .collect();
    // a `threat` with no type is still a match — never read it as safe
    Ok(if types.is_empty() {
        vec!["THREAT_TYPE_UNSPECIFIED".into()]
    } else {
        types
    })
}

#[cfg(target_arch = "wasm32")]
pub async fn lookup(api_key: &str, uri: &str, timeout_ms: u64) -> Result<Vec<String>, RiskError> {
    use futures_util::future::{Either, select};
    use worker::{Delay, Fetch, Headers, Method, Request, RequestInit};

    if api_key.is_empty() {
        return Err(RiskError::MissingKey);
    }
    let headers = Headers::new();
    headers
        .set("x-goog-api-key", api_key)
        .map_err(|_| RiskError::Transport)?;
    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    let req = Request::new_with_init(&search_url(uri), &init).map_err(|_| RiskError::Transport)?;
    let fetch = Box::pin(async move {
        let mut resp = Fetch::Request(req)
            .send()
            .await
            .map_err(|_| RiskError::Transport)?;
        let status = resp.status_code();
        let text = resp.text().await.map_err(|_| RiskError::Transport)?;
        parse(status, &text)
    });
    let timeout = Box::pin(Delay::from(core::time::Duration::from_millis(timeout_ms)));
    match select(fetch, timeout).await {
        Either::Left((r, _)) => r,
        Either::Right(_) => Err(RiskError::Timeout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_has_threat_types_and_encoded_uri_but_no_key() {
        let u = search_url("https://example.com/a?b=1&c=ป");
        assert!(u.starts_with("https://webrisk.googleapis.com/v1/uris:search?"));
        assert_eq!(u.matches("threatTypes=").count(), 3);
        assert!(u.contains("uri=https%3A%2F%2Fexample.com%2Fa%3Fb%3D1%26c%3D"));
        assert!(!u.contains("key="));
    }

    #[test]
    fn parses_match_and_no_match() {
        assert_eq!(parse(200, "{}"), Ok(vec![]));
        assert_eq!(
            parse(
                200,
                r#"{"threat":{"threatTypes":["MALWARE"],"expireTime":"2026-10-10T10:00:00Z"}}"#
            ),
            Ok(vec!["MALWARE".to_string()])
        );
        assert_eq!(
            parse(200, r#"{"threat":{"threatTypes":[]}}"#)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn failures_are_never_safe() {
        // body shape of the live 403 (2026-10-10), trimmed
        let billing = r#"{"error":{"code":403,"status":"PERMISSION_DENIED","details":[{"reason":"BILLING_DISABLED"}]}}"#;
        assert_eq!(parse(403, billing), Err(RiskError::BillingDisabled));
        assert_eq!(
            parse(
                400,
                r#"{"error":{"details":[{"reason":"API_KEY_INVALID"}]}}"#
            ),
            Err(RiskError::Http(400))
        );
        assert_eq!(parse(429, ""), Err(RiskError::Http(429)));
        assert_eq!(parse(200, "nope"), Err(RiskError::BadResponse));
        assert_eq!(parse(200, "[]"), Err(RiskError::BadResponse));
        assert_eq!(
            parse(200, r#"{"unexpected":1}"#),
            Err(RiskError::BadResponse)
        );
        assert_eq!(parse(200, r#"{"threat":{}}"#), Err(RiskError::BadResponse));
    }
}
