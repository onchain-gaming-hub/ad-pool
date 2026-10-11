//! CORS for browser frontends that call the Worker from another origin (e.g. an advertiser
//! page posting `multipart/form-data` to `/v1/creative` and reading the JSON verdict).
//!
//! Opt-in: `CORS_ALLOW_ORIGINS` = comma-separated exact origins (`https://app.example`,
//! `http://localhost:5173`). Unset or empty → no CORS headers at all (previous behavior).
//! No wildcard: the Worker answers with the request's own `Origin` only when it is listed.

/// Methods the Worker serves.
pub const ALLOW_METHODS: &str = "GET, POST, OPTIONS";
/// `multipart/form-data` needs no extra request headers; `content-type` covers clients that
/// set it explicitly, `if-none-match` covers conditional `GET /ads`.
pub const ALLOW_HEADERS: &str = "content-type, if-none-match";
/// Preflight cache, seconds.
pub const MAX_AGE: &str = "600";

/// Parses `CORS_ALLOW_ORIGINS`. Entries are trimmed; empty entries, `*`, and anything that is
/// not a bare `http(s)://host[:port]` origin (path, query, trailing slash) are dropped.
pub fn parse_allowlist(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|o| is_origin(o))
        .map(str::to_string)
        .collect()
}

fn is_origin(o: &str) -> bool {
    let rest = match o
        .strip_prefix("https://")
        .or_else(|| o.strip_prefix("http://"))
    {
        Some(r) => r,
        None => return false,
    };
    !rest.is_empty()
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
}

/// The value for `Access-Control-Allow-Origin`, if this request's `Origin` is allowed.
/// Exact, case-sensitive match (browsers send origins lowercased).
pub fn allowed_origin<'a>(origin: Option<&'a str>, allowlist: &[String]) -> Option<&'a str> {
    let o = origin?;
    allowlist.iter().any(|a| a == o).then_some(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_empty_means_no_cors() {
        assert!(parse_allowlist(None).is_empty());
        assert!(parse_allowlist(Some("")).is_empty());
        assert!(parse_allowlist(Some(" , ")).is_empty());
        assert_eq!(allowed_origin(Some("https://a.example"), &[]), None);
    }

    #[test]
    fn parses_exact_origins_and_drops_junk() {
        let l = parse_allowlist(Some(
            " https://app.example , http://localhost:5173,*,https://x.example/,ftp://f.example,https://y.example/path,https://",
        ));
        assert_eq!(l, vec!["https://app.example", "http://localhost:5173"]);
    }

    #[test]
    fn matches_only_listed_origins() {
        let l = parse_allowlist(Some("https://app.example,http://localhost:5173"));
        assert_eq!(
            allowed_origin(Some("https://app.example"), &l),
            Some("https://app.example")
        );
        assert_eq!(
            allowed_origin(Some("http://localhost:5173"), &l),
            Some("http://localhost:5173")
        );
        assert_eq!(allowed_origin(Some("https://evil.example"), &l), None);
        assert_eq!(allowed_origin(Some("https://app.example.evil"), &l), None);
        assert_eq!(allowed_origin(Some("http://app.example"), &l), None);
        assert_eq!(allowed_origin(Some("null"), &l), None);
        assert_eq!(allowed_origin(None, &l), None);
    }
}
