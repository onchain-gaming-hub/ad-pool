//! `click_url` layer 1 (A14 · W2): rules applied to the submitted URL **and every redirect
//! hop**. WHATWG parsing (`url` crate) so `https://3232235521/`, `https://0x7f.1/` and other
//! IPv4 spellings are seen as IPs, and Unicode hosts are seen in their punycode form.
//! Pure (tested natively); following redirects is Wasm (`follow`).
//! Layer 2 = Web Risk (webrisk.rs) on every hop · layer 3 = FE warning before leaving the game.

use url::{Host, Url};

use crate::accounts::AD_CLICK_URL_MAX_LEN;

/// Redirects followed before giving up (each hop is re-checked by layers 1 + 2).
pub const MAX_REDIRECTS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum UrlReject {
    Unparseable,
    NotHttps,
    UserInfo,
    Port,
    IpHost,
    /// IDN / punycode label (`xn--`) — homograph risk; v1 accepts ASCII hosts only
    Punycode,
    /// single-label, reserved or internal names (localhost, *.local, *.internal, …)
    ReservedHost,
    /// link shorteners hide the destination from review and can be re-pointed later
    Shortener,
    TooLong,
    TooManyRedirects,
    /// a 3xx without a usable Location
    BadRedirect,
}

/// Exact domain or any subdomain of it.
const SHORTENERS: &[&str] = &[
    "bit.ly",
    "bitly.com",
    "tinyurl.com",
    "t.co",
    "goo.gl",
    "ow.ly",
    "is.gd",
    "buff.ly",
    "rebrand.ly",
    "cutt.ly",
    "shorturl.at",
    "tiny.cc",
    "rb.gy",
    "s.id",
    "t.ly",
    "lnkd.in",
    "bl.ink",
    "short.io",
    "v.gd",
    "trib.al",
    "shorte.st",
    "adf.ly",
    "bit.do",
    "tr.im",
    "cli.gs",
    "x.co",
    "qr.ae",
    "u.to",
    "clck.ru",
    "shrtco.de",
    "tiny.one",
    "linktr.ee",
    "lihi.cc",
    "lihi1.cc",
    "reurl.cc",
    "ppt.cc",
];

/// Suffixes that never belong to a public site (RFC 6761 / 6762 / 8375 + common internal).
const RESERVED_SUFFIXES: &[&str] = &[
    "localhost",
    "local",
    "internal",
    "intranet",
    "lan",
    "home",
    "corp",
    "home.arpa",
    "arpa",
    "onion",
    "test",
    "example",
    "invalid",
    "localdomain",
];

fn matches_domain(host: &str, d: &str) -> bool {
    host == d || host.ends_with(&format!(".{d}"))
}

/// Layer 1 on one URL; returns the normalized URL (what goes on chain / is fetched).
pub fn layer1(raw: &str) -> Result<Url, UrlReject> {
    let u = Url::parse(raw).map_err(|_| UrlReject::Unparseable)?;
    if u.scheme() != "https" {
        return Err(UrlReject::NotHttps);
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(UrlReject::UserInfo);
    }
    // the parser drops the default port, so any port here is non-443
    if u.port().is_some() {
        return Err(UrlReject::Port);
    }
    let host = match u.host() {
        Some(Host::Domain(d)) => d.trim_end_matches('.').to_ascii_lowercase(),
        Some(Host::Ipv4(_) | Host::Ipv6(_)) => return Err(UrlReject::IpHost),
        None => return Err(UrlReject::Unparseable),
    };
    if host.split('.').any(|l| l.starts_with("xn--")) {
        return Err(UrlReject::Punycode);
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2
        || labels.iter().any(|l| l.is_empty())
        // an all-numeric TLD is an IP spelling the parser did not catch
        || labels.last().is_some_and(|t| t.bytes().all(|b| b.is_ascii_digit()))
        || RESERVED_SUFFIXES.iter().any(|s| matches_domain(&host, s))
    {
        return Err(UrlReject::ReservedHost);
    }
    if SHORTENERS.iter().any(|s| matches_domain(&host, s)) {
        return Err(UrlReject::Shortener);
    }
    if u.as_str().len() > AD_CLICK_URL_MAX_LEN {
        return Err(UrlReject::TooLong);
    }
    Ok(u)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hop {
    /// not a redirect — this URL is the destination
    Final,
    Redirect(Url),
}

/// What a response means for the redirect chain (`redirect: manual`).
pub fn next_hop(current: &Url, status: u16, location: Option<&str>) -> Result<Hop, UrlReject> {
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return Ok(Hop::Final);
    }
    let loc = location.ok_or(UrlReject::BadRedirect)?;
    let next = current.join(loc).map_err(|_| UrlReject::BadRedirect)?;
    // re-checked by layer 1 (normalizes the same way as a submitted URL)
    Ok(Hop::Redirect(layer1(next.as_str())?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_https_and_normalizes() {
        assert_eq!(
            layer1("https://Example.COM").unwrap().as_str(),
            "https://example.com/"
        );
        assert_eq!(
            layer1("https://shop.example.co.th/a?b=1#c")
                .unwrap()
                .as_str(),
            "https://shop.example.co.th/a?b=1#c"
        );
        // explicit default port is dropped by the parser → allowed
        assert!(layer1("https://example.org:443/").is_ok());
        // Thai path is percent-encoded, host stays ASCII
        let u = layer1("https://example.org/ป้าย").unwrap();
        assert!(u.as_str().is_ascii());
    }

    #[test]
    fn rejects_by_rule() {
        use UrlReject::*;
        for (u, want) in [
            ("http://example.com/", NotHttps),
            ("javascript:alert(1)", NotHttps),
            ("https://user:pw@example.com/", UserInfo),
            ("https://example.com@evil.example.org/", UserInfo),
            ("https://example.com:8443/", Port),
            ("https://127.0.0.1/", IpHost),
            ("https://3232235521/", IpHost),
            ("https://0x7f.1/", IpHost),
            ("https://[::1]/", IpHost),
            ("https://xn--80ak6aa92e.com/", Punycode),
            ("https://аррӏе.com/", Punycode),
            ("https://ไทย.ไทย/", Punycode),
            ("https://localhost/", ReservedHost),
            ("https://intranet/", ReservedHost),
            ("https://printer.local/", ReservedHost),
            ("https://svc.internal/", ReservedHost),
            ("https://a.b.home.arpa/", ReservedHost),
            ("https://bit.ly/x", Shortener),
            ("https://www.bit.ly/x", Shortener),
            ("https://T.CO/x", Shortener),
            ("not a url", Unparseable),
        ] {
            assert_eq!(layer1(u), Err(want), "{u}");
        }
        assert_eq!(
            layer1(&format!("https://example.com/{}", "a".repeat(200))),
            Err(TooLong)
        );
        // look-alikes of a shortener are not shorteners
        assert!(layer1("https://notbit.ly/").is_ok());
    }

    #[test]
    fn redirect_hops() {
        let base = layer1("https://example.com/a/b").unwrap();
        assert_eq!(next_hop(&base, 200, None), Ok(Hop::Final));
        assert_eq!(
            next_hop(&base, 404, Some("https://x.example.org/")),
            Ok(Hop::Final)
        );
        assert_eq!(
            next_hop(&base, 302, Some("../c")),
            Ok(Hop::Redirect(Url::parse("https://example.com/c").unwrap()))
        );
        assert_eq!(next_hop(&base, 301, None), Err(UrlReject::BadRedirect));
        // a redirect to a blocked destination fails layer 1
        assert_eq!(
            next_hop(&base, 307, Some("http://example.com/")),
            Err(UrlReject::NotHttps)
        );
        assert_eq!(
            next_hop(&base, 308, Some("https://10.0.0.1/")),
            Err(UrlReject::IpHost)
        );
        assert_eq!(
            next_hop(&base, 302, Some("https://bit.ly/z")),
            Err(UrlReject::Shortener)
        );
    }
}
