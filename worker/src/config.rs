//! Worker configuration from `wrangler.toml` [vars] + secrets. Parsing is pure (tested).

use solana_pubkey::Pubkey;

/// devnet genesis hash — hard-coded: the devnet-only rule must not be one editable var away.
pub const DEVNET_GENESIS_HASH: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";
pub const MIN_RPC_TIMEOUT_MS: u64 = 1_000;
/// Uploaded image cap (bytes). OpenAI accepts up to 20 MB; ads are small and fixed-size.
pub const MAX_IMAGE_BYTES_CAP: usize = 5 * 1024 * 1024;
pub const MAX_SUBMISSIONS_CAP: usize = 60;
pub const MAX_RPC_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub cluster: String,
    pub expected_genesis_hash: String,
    pub ad_pool: Pubkey,
    pub platform_core: Pubkey,
    pub rpc_timeout_ms: u64,
    /// `domain:` line every signed submission must carry (binds signatures to this site).
    pub auth_domain: String,
    pub max_image_bytes: usize,
    /// per auction per hour (design §7.3)
    pub max_submissions_per_hour: usize,
    /// Pubkey the `MODERATOR_KEYPAIR` secret must hold (catches a wrong / rotated secret).
    pub moderator: Pubkey,
    /// Pubkey of `CRANK_KEYPAIR` (W3: no role, fee SOL only — settle / close_auction).
    pub crank: Pubkey,
    /// Origin the Worker serves `GET /ads/<sha256>.<ext>` from; `image_url` on chain =
    /// `<PUBLIC_BASE_URL>/ads/<sha256>.<ext>`. https, no trailing slash.
    pub public_base_url: String,
}

/// `image_url` ≤ 200 bytes on chain: base + "/ads/" + 64 hex + ".webp".
pub const MAX_PUBLIC_BASE_URL_LEN: usize = 200 - "/ads/".len() - 64 - ".webp".len();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

/// `get(name)` returns a var/secret value.
pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
    let req = |n: &str| get(n).ok_or_else(|| ConfigError(format!("missing {n}")));
    let key = |n: &str| -> Result<Pubkey, ConfigError> {
        req(n)?
            .parse()
            .map_err(|_| ConfigError(format!("{n} is not a pubkey")))
    };
    let cluster = req("CLUSTER")?;
    if cluster != "devnet" {
        // Golden rule: devnet only unless maintainer says otherwise.
        return Err(ConfigError(format!("CLUSTER {cluster} not allowed")));
    }
    let expected_genesis_hash = req("EXPECTED_GENESIS_HASH")?;
    if expected_genesis_hash != DEVNET_GENESIS_HASH {
        return Err(ConfigError("EXPECTED_GENESIS_HASH is not devnet".into()));
    }
    let rpc_timeout_ms: u64 = req("RPC_TIMEOUT_MS")?
        .parse()
        .map_err(|_| ConfigError("RPC_TIMEOUT_MS".into()))?;
    if !(MIN_RPC_TIMEOUT_MS..=MAX_RPC_TIMEOUT_MS).contains(&rpc_timeout_ms) {
        return Err(ConfigError("RPC_TIMEOUT_MS out of range".into()));
    }
    let auth_domain = req("AUTH_DOMAIN")?;
    if auth_domain.is_empty() || auth_domain.contains(char::is_whitespace) {
        return Err(ConfigError("AUTH_DOMAIN".into()));
    }
    let num = |n: &str, lo: usize, hi: usize| -> Result<usize, ConfigError> {
        let v: usize = req(n)?.parse().map_err(|_| ConfigError(n.to_string()))?;
        if !(lo..=hi).contains(&v) {
            return Err(ConfigError(format!("{n} out of range")));
        }
        Ok(v)
    };
    let max_image_bytes = num("MAX_IMAGE_BYTES", 1_024, MAX_IMAGE_BYTES_CAP)?;
    let max_submissions_per_hour = num("MAX_SUBMISSIONS_PER_HOUR", 1, MAX_SUBMISSIONS_CAP)?;
    let public_base_url = req("PUBLIC_BASE_URL")?;
    let host = public_base_url.strip_prefix("https://").unwrap_or("");
    if host.is_empty()
        || public_base_url.len() > MAX_PUBLIC_BASE_URL_LEN
        || public_base_url.ends_with('/')
        || !public_base_url.is_ascii()
        || public_base_url
            .contains(|c: char| c.is_whitespace() || c.is_control() || c == '?' || c == '#')
    {
        return Err(ConfigError("PUBLIC_BASE_URL".into()));
    }
    let moderator = key("MODERATOR_ADDRESS")?;
    let crank = key("CRANK_ADDRESS")?;
    if moderator == crank {
        // the permissionless crank must never run on the most powerful key (design §6.1)
        return Err(ConfigError(
            "CRANK_ADDRESS must differ from MODERATOR_ADDRESS".into(),
        ));
    }
    Ok(Config {
        moderator,
        crank,
        public_base_url,
        auth_domain,
        max_image_bytes,
        max_submissions_per_hour,
        cluster,
        expected_genesis_hash,
        ad_pool: key("AD_POOL_PROGRAM_ID")?,
        platform_core: key("PLATFORM_CORE_PROGRAM_ID")?,
        rpc_timeout_ms,
    })
}

/// The keyed RPC URL must be https (it carries an API key).
pub fn validate_rpc_url(url: &str) -> Result<(), ConfigError> {
    if !url.starts_with("https://") || url.len() <= "https://".len() {
        return Err(ConfigError("RPC URL must be https://".into()));
    }
    Ok(())
}

/// Secret name of the keyed RPC URL for the cluster.
pub fn rpc_secret_name(cluster: &str) -> String {
    format!("RPC_URL_{}", cluster.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn vars() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            ("CLUSTER", "devnet"),
            (
                "EXPECTED_GENESIS_HASH",
                "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG",
            ),
            (
                "AD_POOL_PROGRAM_ID",
                "6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk",
            ),
            (
                "PLATFORM_CORE_PROGRAM_ID",
                "AJnPSYsn7J2muHfKztRtpZz5kBP94dWkNZLr9aMhu18G",
            ),
            ("RPC_TIMEOUT_MS", "10000"),
            ("AUTH_DOMAIN", "ads.devnet.example"),
            ("MAX_IMAGE_BYTES", "1048576"),
            ("MAX_SUBMISSIONS_PER_HOUR", "10"),
            (
                "MODERATOR_ADDRESS",
                "DX6bYh6AwfWpBQT8UBevNocYepauHd965RCr1rE6G6Db",
            ),
            ("PUBLIC_BASE_URL", "https://ads.devnet.example"),
            (
                "CRANK_ADDRESS",
                "ESgASsgG2Uzh4ccQTga5yXoZnkDCCEociVYYe6Trq3LP",
            ),
        ])
    }

    #[test]
    fn parses_wrangler_vars() {
        let v = vars();
        let c = from_lookup(|n| v.get(n).map(|s| s.to_string())).unwrap();
        assert_eq!(
            c.ad_pool.to_string(),
            "6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk"
        );
        assert_eq!(c.rpc_timeout_ms, 10_000);
        assert_eq!(rpc_secret_name(&c.cluster), "RPC_URL_DEVNET");
    }

    #[test]
    fn refuses_non_devnet_and_bad_values() {
        let mut v = vars();
        v.insert("CLUSTER", "mainnet");
        assert!(from_lookup(|n| v.get(n).map(|s| s.to_string())).is_err());
        let mut v = vars();
        v.insert("AD_POOL_PROGRAM_ID", "not-a-key");
        assert!(from_lookup(|n| v.get(n).map(|s| s.to_string())).is_err());
        let mut v = vars();
        v.remove("RPC_TIMEOUT_MS");
        assert!(from_lookup(|n| v.get(n).map(|s| s.to_string())).is_err());
        for t in ["0", "999", "30001"] {
            let mut v = vars();
            v.insert("RPC_TIMEOUT_MS", t);
            assert!(
                from_lookup(|n| v.get(n).map(|s| s.to_string())).is_err(),
                "{t}"
            );
        }
        for b in [
            "http://ads.example",
            "https://",
            "https://ads.example/",
            "https://a b",
            "https://x?y",
        ] {
            let mut v = vars();
            v.insert("PUBLIC_BASE_URL", b);
            assert!(
                from_lookup(|n| v.get(n).map(|s| s.to_string())).is_err(),
                "{b}"
            );
        }
        let long = format!("https://{}", "a".repeat(MAX_PUBLIC_BASE_URL_LEN));
        let mut v = vars();
        v.insert("PUBLIC_BASE_URL", &long);
        assert!(from_lookup(|n| v.get(n).map(|s| s.to_string())).is_err());
        let mut v = vars();
        v.insert(
            "CRANK_ADDRESS",
            "DX6bYh6AwfWpBQT8UBevNocYepauHd965RCr1rE6G6Db",
        );
        assert!(
            from_lookup(|n| v.get(n).map(|s| s.to_string())).is_err(),
            "crank = moderator"
        );
        // another cluster's hash with a "devnet" label must not pass
        let mut v = vars();
        v.insert(
            "EXPECTED_GENESIS_HASH",
            "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d",
        );
        assert!(from_lookup(|n| v.get(n).map(|s| s.to_string())).is_err());
    }

    #[test]
    fn rpc_url_must_be_https() {
        assert!(validate_rpc_url("https://rpc.example/key").is_ok());
        assert!(validate_rpc_url("http://rpc.example/key").is_err());
        assert!(validate_rpc_url("rpc.example").is_err());
        assert!(validate_rpc_url("https://").is_err());
    }

    #[test]
    fn wrangler_toml_vars_parse() {
        // keep wrangler.toml and the parser in sync
        let toml =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/wrangler.toml")).unwrap();
        let get = |n: &str| {
            toml.lines()
                .find_map(|l| l.strip_prefix(&format!("{n} = ")))
                .map(|v| v.trim().trim_matches('"').to_string())
        };
        let c = from_lookup(get).unwrap();
        assert_eq!(
            c.platform_core.to_string(),
            "AJnPSYsn7J2muHfKztRtpZz5kBP94dWkNZLr9aMhu18G"
        );
        // no secret-looking key may be an active line in wrangler.toml
        for line in toml
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with('#') && l.contains('='))
        {
            let key = line.split('=').next().unwrap().trim().to_uppercase();
            for bad in ["KEY", "SECRET", "KEYPAIR", "RPC_URL", "TOKEN"] {
                assert!(
                    !key.contains(bad),
                    "secret-looking var in wrangler.toml: {line}"
                );
            }
        }
    }
}
