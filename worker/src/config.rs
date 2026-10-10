//! Worker configuration from `wrangler.toml` [vars] + secrets. Parsing is pure (tested).

use solana_pubkey::Pubkey;

/// devnet genesis hash — hard-coded: the devnet-only rule must not be one editable var away.
pub const DEVNET_GENESIS_HASH: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";
pub const MIN_RPC_TIMEOUT_MS: u64 = 1_000;
pub const MAX_RPC_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub cluster: String,
    pub expected_genesis_hash: String,
    pub ad_pool: Pubkey,
    pub platform_core: Pubkey,
    pub rpc_timeout_ms: u64,
}

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
    Ok(Config {
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
