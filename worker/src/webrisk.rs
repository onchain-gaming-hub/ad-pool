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

/// A "no match" answer has no `expireTime`; it is reused this long (W3 re-check runs every
/// 10 min — so a URL turning bad is seen within ~40 min).
pub const SAFE_CACHE_SECS: i64 = 1_800;
/// A match is reused until its `expireTime`, capped here (and 1 h if it has none).
pub const MAX_THREAT_CACHE_SECS: i64 = 86_400;
pub const THREAT_CACHE_DEFAULT_SECS: i64 = 3_600;

/// One cached answer (Durable Object `RiskCache`, key = `cache_key(uri)`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CacheEntry {
    pub threats: Vec<String>,
    pub until: i64,
}

/// sha256 hex of the URI — the cache never stores the URL itself as a key.
pub fn cache_key(uri: &str) -> String {
    crate::auth::hex(&crate::auth::sha256(uri.as_bytes()))
}

pub fn cache_until(now: i64, threats: &[String], expire_ts: Option<i64>) -> i64 {
    if threats.is_empty() {
        return now + SAFE_CACHE_SECS;
    }
    match expire_ts {
        Some(t) if t > now => t.min(now + MAX_THREAT_CACHE_SECS),
        Some(_) => now,
        None => now + THREAT_CACHE_DEFAULT_SECS,
    }
}

/// `YYYY-MM-DDTHH:MM:SS[.frac]Z` (what Web Risk returns, UTC) → unix seconds.
pub fn rfc3339_utc(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let time = time.split('.').next()?;
    let d: Vec<i64> = date
        .split('-')
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    let t: Vec<i64> = time
        .split(':')
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    let ([y, m, day], [hh, mm, ss]) = (d.as_slice(), t.as_slice()) else {
        return None;
    };
    if !(1..=12).contains(m) || !(1..=31).contains(day) || *hh > 23 || *mm > 59 || *ss > 60 {
        return None;
    }
    // days from civil (H. Hinnant)
    let y = if *m <= 2 { y - 1 } else { *y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3_600 + mm * 60 + ss)
}

/// `threat.expireTime` of a 200 body, if any.
pub fn expire_ts(body: &str) -> Option<i64> {
    let v: Value = serde_json::from_str(body).ok()?;
    rfc3339_utc(v.get("threat")?.get("expireTime")?.as_str()?)
}

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
pub fn search_url(base: &str, uri: &str) -> String {
    let Ok(mut u) = url::Url::parse(base) else {
        return String::new();
    };
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
/// `(threat types, expireTime)` — empty types = no match.
pub async fn lookup(
    base: &str,
    api_key: &str,
    uri: &str,
    timeout_ms: u64,
) -> Result<(Vec<String>, Option<i64>), RiskError> {
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
    let req =
        Request::new_with_init(&search_url(base, uri), &init).map_err(|_| RiskError::Transport)?;
    let fetch = Box::pin(async move {
        let mut resp = Fetch::Request(req)
            .send()
            .await
            .map_err(|_| RiskError::Transport)?;
        let status = resp.status_code();
        let text = resp.text().await.map_err(|_| RiskError::Transport)?;
        Ok((parse(status, &text)?, expire_ts(&text)))
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
        let u = search_url(SEARCH_URL, "https://example.com/a?b=1&c=ป");
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
    fn expire_time_and_cache_lifetimes() {
        assert_eq!(rfc3339_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            rfc3339_utc("2026-10-10T10:23:53.310234623Z"),
            Some(1_791_627_833)
        );
        assert_eq!(rfc3339_utc("2000-02-29T12:00:00Z"), Some(951_825_600));
        for bad in [
            "2026-10-10T10:23:53",
            "2026-13-01T00:00:00Z",
            "x",
            "2026-10-10 10:23:53Z",
        ] {
            assert_eq!(rfc3339_utc(bad), None, "{bad}");
        }
        let body =
            r#"{"threat":{"threatTypes":["MALWARE"],"expireTime":"2026-10-10T10:23:53.3Z"}}"#;
        assert_eq!(expire_ts(body), Some(1_791_627_833));
        assert_eq!(expire_ts("{}"), None);
        let now = 1_000;
        let bad = vec!["MALWARE".to_string()];
        assert_eq!(cache_until(now, &[], None), now + SAFE_CACHE_SECS);
        assert_eq!(cache_until(now, &bad, Some(now + 300)), now + 300);
        assert_eq!(
            cache_until(now, &bad, Some(now + 10 * 86_400)),
            now + MAX_THREAT_CACHE_SECS
        );
        assert_eq!(
            cache_until(now, &bad, None),
            now + THREAT_CACHE_DEFAULT_SECS
        );
        assert_eq!(cache_until(now, &bad, Some(now - 5)), now);
        assert_eq!(cache_key("https://a.example.com/").len(), 64);
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
