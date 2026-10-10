//! Minimal Solana JSON-RPC: request bodies + response parsing are pure (unit-tested
//! natively); the HTTP call (`RpcClient`, Wasm only) races every fetch against a timeout,
//! because `fetch` has none (solana-poc gotcha). Values are cached per request, never
//! futures (Workers forbid cross-request I/O).

use base64::Engine;
use serde_json::{Value, json};

/// ⚠️ `Http` text may contain the keyed RPC URL (= a secret). Never return it to clients —
/// use `public_code()`; log only `redact_url()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcError {
    Http(String),
    Timeout,
    Rpc { code: i64, message: String },
    BadResponse(String),
    WrongCluster { expected: String, got: String },
}

impl RpcError {
    /// Safe, generic code for HTTP responses.
    pub fn public_code(&self) -> &'static str {
        match self {
            RpcError::Http(_) => "rpc_unavailable",
            RpcError::Timeout => "rpc_timeout",
            RpcError::Rpc { .. } => "rpc_error",
            RpcError::BadResponse(_) => "rpc_bad_response",
            RpcError::WrongCluster { .. } => "rpc_wrong_cluster",
        }
    }
}

/// `scheme://host` only — drops path / query (where providers put API keys) and userinfo.
pub fn redact_url(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("?", url));
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    format!("{scheme}://{host}")
}

impl core::fmt::Display for RpcError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// An account as returned by `getAccountInfo` (base64 encoding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    pub lamports: u64,
    pub owner: solana_pubkey::Pubkey,
    pub executable: bool,
    pub data: Vec<u8>,
}

pub fn body(id: u64, method: &str, params: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()
}

/// `confirmed` is enough for the Worker: the fields it acts on (`highest_bidder`, sizes)
/// are frozen once bidding closes at `close_ts`, before the creative window opens.
pub fn get_account_info_body(id: u64, address: &str) -> String {
    body(
        id,
        "getAccountInfo",
        json!([address, { "encoding": "base64", "commitment": "confirmed" }]),
    )
}

pub fn get_genesis_hash_body(id: u64) -> String {
    body(id, "getGenesisHash", json!([]))
}

/// `result` of a JSON-RPC response, or the RPC error.
pub fn result(response: &str) -> Result<Value, RpcError> {
    let v: Value =
        serde_json::from_str(response).map_err(|e| RpcError::BadResponse(e.to_string()))?;
    if let Some(err) = v.get("error") {
        return Err(RpcError::Rpc {
            code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        });
    }
    v.get("result")
        .cloned()
        .ok_or_else(|| RpcError::BadResponse("no result".into()))
}

/// `None` = account does not exist.
pub fn parse_account_info(response: &str) -> Result<Option<AccountInfo>, RpcError> {
    let r = result(response)?;
    let value = r
        .get("value")
        .ok_or_else(|| RpcError::BadResponse("no value".into()))?;
    if value.is_null() {
        return Ok(None);
    }
    let bad = |m: &str| RpcError::BadResponse(m.to_string());
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("data"))?;
    if data.get(1).and_then(Value::as_str) != Some("base64") {
        return Err(bad("encoding"));
    }
    let b64 = data
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| bad("data[0]"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| bad("base64"))?;
    Ok(Some(AccountInfo {
        lamports: value
            .get("lamports")
            .and_then(Value::as_u64)
            .ok_or_else(|| bad("lamports"))?,
        owner: value
            .get("owner")
            .and_then(Value::as_str)
            .and_then(|o| o.parse().ok())
            .ok_or_else(|| bad("owner"))?,
        executable: value
            .get("executable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        data: bytes,
    }))
}

pub fn parse_genesis_hash(response: &str) -> Result<String, RpcError> {
    result(response)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| RpcError::BadResponse("genesis hash".into()))
}

pub fn get_latest_blockhash_body(id: u64) -> String {
    body(
        id,
        "getLatestBlockhash",
        json!([{ "commitment": "confirmed" }]),
    )
}

/// `(blockhash, lastValidBlockHeight)`
pub fn parse_latest_blockhash(response: &str) -> Result<(solana_hash::Hash, u64), RpcError> {
    let r = result(response)?;
    let v = r
        .get("value")
        .ok_or_else(|| RpcError::BadResponse("no value".into()))?;
    let hash = v
        .get("blockhash")
        .and_then(Value::as_str)
        .and_then(|h| h.parse().ok())
        .ok_or_else(|| RpcError::BadResponse("blockhash".into()))?;
    let height = v
        .get("lastValidBlockHeight")
        .and_then(Value::as_u64)
        .ok_or_else(|| RpcError::BadResponse("lastValidBlockHeight".into()))?;
    Ok((hash, height))
}

/// Preflight stays ON: the program's own checks (status, window, role) reject a stale
/// decision before it costs a fee.
pub fn send_transaction_body(id: u64, tx_base64: &str) -> String {
    body(
        id,
        "sendTransaction",
        json!([tx_base64, { "encoding": "base64", "skipPreflight": false, "preflightCommitment": "confirmed" }]),
    )
}

pub fn get_signature_statuses_body(id: u64, signature: &str) -> String {
    body(
        id,
        "getSignatureStatuses",
        json!([[signature], { "searchTransactionHistory": false }]),
    )
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum SendOutcome {
    /// the RPC accepted the tx and returned its signature
    Accepted(String),
    /// definitely not forwarded (JSON-RPC error / HTTP 4xx). `custom` = preflight
    /// `InstructionError[i, {Custom: c}]` (the program's error code, e.g. 6012)
    Rejected {
        reason: String,
        http_429: bool,
        tx_fault: bool,
        custom: Option<(usize, u32)>,
    },
    /// timeout / transport / 5xx / unparseable 200 — it may have been forwarded
    Unknown(String),
    AlreadyProcessed,
}

/// JSON-RPC errors raised by the node rather than by the tx (solana-poc `rpc.rs`).
const NODE_SIDE_ERROR_CODES: [i64; 3] = [-32603, -32005, -32004];

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `{"InstructionError":[i,{"Custom":c}]}` → `(i, c)`
pub fn custom_error(err: &Value) -> Option<(usize, u32)> {
    let ie = err.get("InstructionError")?.as_array()?;
    let index = usize::try_from(ie.first()?.as_u64()?).ok()?;
    let code = u32::try_from(ie.get(1)?.get("Custom")?.as_u64()?).ok()?;
    Some((index, code))
}

/// Port of `solana-poc/relayer/src/rpc.rs::classify_send`.
pub fn classify_send(status: u16, text: &str) -> SendOutcome {
    if (400..500).contains(&status) {
        return SendOutcome::Rejected {
            reason: format!("http {status}"),
            http_429: status == 429,
            tx_fault: matches!(status, 400 | 413),
            custom: None,
        };
    }
    if status != 200 {
        return SendOutcome::Unknown(format!("http {status}"));
    }
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return SendOutcome::Unknown("unparseable response".into());
    };
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("rpc error");
        if msg.contains("already been processed") || msg.contains("AlreadyProcessed") {
            return SendOutcome::AlreadyProcessed;
        }
        let code = err.get("code").and_then(Value::as_i64);
        return SendOutcome::Rejected {
            reason: truncate(msg, 200),
            http_429: false,
            tx_fault: code.is_some_and(|c| !NODE_SIDE_ERROR_CODES.contains(&c)),
            custom: err
                .get("data")
                .and_then(|d| d.get("err"))
                .and_then(custom_error),
        };
    }
    match v.get("result").and_then(Value::as_str) {
        Some(sig) => SendOutcome::Accepted(sig.to_string()),
        None => SendOutcome::Unknown("missing result".into()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum TxStatus {
    /// not seen yet, or seen but only `processed`
    Pending,
    Confirmed,
    Failed {
        custom: Option<(usize, u32)>,
        err: String,
    },
}

pub fn parse_signature_status(response: &str) -> Result<TxStatus, RpcError> {
    let r = result(response)?;
    let first = r
        .get("value")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .ok_or_else(|| RpcError::BadResponse("value".into()))?;
    if first.is_null() {
        return Ok(TxStatus::Pending);
    }
    if let Some(err) = first.get("err").filter(|e| !e.is_null()) {
        return Ok(TxStatus::Failed {
            custom: custom_error(err),
            err: truncate(&err.to_string(), 200),
        });
    }
    Ok(
        match first.get("confirmationStatus").and_then(Value::as_str) {
            Some("confirmed" | "finalized") => TxStatus::Confirmed,
            _ => TxStatus::Pending,
        },
    )
}

#[cfg(target_arch = "wasm32")]
pub use client::RpcClient;

#[cfg(target_arch = "wasm32")]
mod client {
    use super::*;
    use futures_util::future::{Either, select};
    use worker::{Delay, Fetch, Headers, Method, Request, RequestInit};

    pub struct RpcClient {
        pub url: String,
        pub timeout_ms: u64,
    }

    impl RpcClient {
        /// `(status, body)` of any HTTP response — `sendTransaction` classifies it itself.
        async fn post_raw(&self, body: String) -> Result<(u16, String), RpcError> {
            let headers = Headers::new();
            headers
                .set("content-type", "application/json")
                .map_err(|e| RpcError::Http(e.to_string()))?;
            let mut init = RequestInit::new();
            init.with_method(Method::Post)
                .with_headers(headers)
                .with_body(Some(body.into()));
            let req = Request::new_with_init(&self.url, &init)
                .map_err(|e| RpcError::Http(e.to_string()))?;
            let fetch = Box::pin(async move {
                let mut resp = Fetch::Request(req)
                    .send()
                    .await
                    .map_err(|e| RpcError::Http(e.to_string()))?;
                let status = resp.status_code();
                let text = resp
                    .text()
                    .await
                    .map_err(|e| RpcError::Http(e.to_string()))?;
                Ok((status, text))
            });
            let timeout = Box::pin(Delay::from(core::time::Duration::from_millis(
                self.timeout_ms,
            )));
            match select(fetch, timeout).await {
                Either::Left((r, _)) => r,
                Either::Right(_) => Err(RpcError::Timeout),
            }
        }

        pub async fn get_latest_blockhash(&self) -> Result<(solana_hash::Hash, u64), RpcError> {
            parse_latest_blockhash(&self.post(get_latest_blockhash_body(1)).await?)
        }

        /// ⚠️ `Unknown` text may hold transport detail with the URL — log only redacted.
        pub async fn send_transaction(&self, tx_base64: &str) -> SendOutcome {
            match self.post_raw(send_transaction_body(1, tx_base64)).await {
                Ok((status, text)) => classify_send(status, &text),
                Err(RpcError::Timeout) => SendOutcome::Unknown("timeout".into()),
                Err(_) => SendOutcome::Unknown("transport".into()),
            }
        }

        pub async fn get_signature_status(&self, signature: &str) -> Result<TxStatus, RpcError> {
            parse_signature_status(&self.post(get_signature_statuses_body(1, signature)).await?)
        }

        async fn post(&self, body: String) -> Result<String, RpcError> {
            let headers = Headers::new();
            headers
                .set("content-type", "application/json")
                .map_err(|e| RpcError::Http(e.to_string()))?;
            let mut init = RequestInit::new();
            init.with_method(Method::Post)
                .with_headers(headers)
                .with_body(Some(body.into()));
            let req = Request::new_with_init(&self.url, &init)
                .map_err(|e| RpcError::Http(e.to_string()))?;
            let fetch = Box::pin(async move {
                let mut resp = Fetch::Request(req)
                    .send()
                    .await
                    .map_err(|e| RpcError::Http(e.to_string()))?;
                if resp.status_code() != 200 {
                    return Err(RpcError::Http(format!("status {}", resp.status_code())));
                }
                resp.text().await.map_err(|e| RpcError::Http(e.to_string()))
            });
            let timeout = Box::pin(Delay::from(core::time::Duration::from_millis(
                self.timeout_ms,
            )));
            match select(fetch, timeout).await {
                Either::Left((r, _)) => r,
                Either::Right(_) => Err(RpcError::Timeout),
            }
        }

        pub async fn get_account_info(
            &self,
            address: &str,
        ) -> Result<Option<AccountInfo>, RpcError> {
            parse_account_info(&self.post(get_account_info_body(1, address)).await?)
        }

        /// Refuse to work against the wrong cluster (e.g. mainnet RPC with devnet config).
        pub async fn ensure_cluster(&self, expected_genesis: &str) -> Result<(), RpcError> {
            let got = parse_genesis_hash(&self.post(get_genesis_hash_body(1)).await?)?;
            if got != expected_genesis {
                return Err(RpcError::WrongCluster {
                    expected: expected_genesis.into(),
                    got,
                });
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_requests() {
        let b: Value = serde_json::from_str(&get_account_info_body(7, "Abc")).unwrap();
        assert_eq!(b["method"], "getAccountInfo");
        assert_eq!(b["id"], 7);
        assert_eq!(b["params"][0], "Abc");
        assert_eq!(b["params"][1]["encoding"], "base64");
        let g: Value = serde_json::from_str(&get_genesis_hash_body(1)).unwrap();
        assert_eq!(g["method"], "getGenesisHash");
    }

    #[test]
    fn parses_account_info() {
        let ok = r#"{"jsonrpc":"2.0","result":{"context":{"slot":1},"value":{"data":["AQID","base64"],"executable":false,"lamports":42,"owner":"11111111111111111111111111111111","rentEpoch":0,"space":3}},"id":1}"#;
        let a = parse_account_info(ok).unwrap().unwrap();
        assert_eq!(
            (a.lamports, a.data, a.executable),
            (42, vec![1, 2, 3], false)
        );
        assert_eq!(a.owner.to_string(), "11111111111111111111111111111111");
        let none = r#"{"jsonrpc":"2.0","result":{"context":{"slot":1},"value":null},"id":1}"#;
        assert_eq!(parse_account_info(none).unwrap(), None);
        let err = r#"{"jsonrpc":"2.0","error":{"code":-32602,"message":"Invalid param"},"id":1}"#;
        assert_eq!(
            parse_account_info(err),
            Err(RpcError::Rpc {
                code: -32602,
                message: "Invalid param".into()
            })
        );
        let wrong_enc = r#"{"jsonrpc":"2.0","result":{"value":{"data":["x","jsonParsed"],"lamports":1,"owner":"1"}},"id":1}"#;
        assert!(matches!(
            parse_account_info(wrong_enc),
            Err(RpcError::BadResponse(_))
        ));
        assert!(matches!(
            parse_account_info("not json"),
            Err(RpcError::BadResponse(_))
        ));
    }
    #[test]
    fn redacts_urls_and_maps_errors_to_public_codes() {
        assert_eq!(
            redact_url("https://devnet.helius-rpc.com/?api-key=SECRET"),
            "https://devnet.helius-rpc.com"
        );
        assert_eq!(
            redact_url("https://solana-devnet.g.alchemy.com/v2/SECRETKEY"),
            "https://solana-devnet.g.alchemy.com"
        );
        assert_eq!(
            redact_url("https://user:pw@rpc.example/x"),
            "https://rpc.example"
        );
        let e = RpcError::Http("Fetch API cannot load: https://x/v2/SECRET".into());
        assert_eq!(e.public_code(), "rpc_unavailable");
    }

    #[test]
    fn rpc_error_wins_and_bad_owner_is_rejected() {
        let both = r#"{"jsonrpc":"2.0","result":"x","error":{"code":-1,"message":"m"},"id":1}"#;
        assert!(matches!(result(both), Err(RpcError::Rpc { code: -1, .. })));
        let bad_owner = r#"{"jsonrpc":"2.0","result":{"value":{"data":["AQ==","base64"],"lamports":1,"owner":"not-a-key"}},"id":1}"#;
        assert!(matches!(
            parse_account_info(bad_owner),
            Err(RpcError::BadResponse(_))
        ));
    }

    #[test]
    fn parses_genesis_hash() {
        let r =
            r#"{"jsonrpc":"2.0","result":"EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG","id":1}"#;
        assert_eq!(
            parse_genesis_hash(r).unwrap(),
            "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG"
        );
    }
    #[test]
    fn parses_blockhash_and_statuses() {
        let r = r#"{"jsonrpc":"2.0","result":{"context":{"slot":1},"value":{"blockhash":"EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG","lastValidBlockHeight":300}},"id":1}"#;
        let (h, n) = parse_latest_blockhash(r).unwrap();
        assert_eq!(
            (h.to_string().as_str(), n),
            ("EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG", 300)
        );
        let st = |v: &str| {
            parse_signature_status(&format!(
                r#"{{"jsonrpc":"2.0","result":{{"context":{{"slot":1}},"value":[{v}]}},"id":1}}"#
            ))
            .unwrap()
        };
        assert_eq!(st("null"), TxStatus::Pending);
        assert_eq!(
            st(r#"{"slot":1,"confirmations":0,"err":null,"confirmationStatus":"processed"}"#),
            TxStatus::Pending
        );
        assert_eq!(
            st(r#"{"slot":1,"confirmations":null,"err":null,"confirmationStatus":"finalized"}"#),
            TxStatus::Confirmed
        );
        assert_eq!(
            st(r#"{"slot":1,"confirmations":1,"err":null,"confirmationStatus":"confirmed"}"#),
            TxStatus::Confirmed
        );
        assert!(matches!(
            st(
                r#"{"slot":1,"err":{"InstructionError":[0,{"Custom":6012}]},"confirmationStatus":"confirmed"}"#
            ),
            TxStatus::Failed {
                custom: Some((0, 6012)),
                ..
            }
        ));
    }

    #[test]
    fn classifies_send() {
        assert_eq!(
            classify_send(200, r#"{"jsonrpc":"2.0","result":"5sig","id":1}"#),
            SendOutcome::Accepted("5sig".into())
        );
        assert!(matches!(
            classify_send(429, ""),
            SendOutcome::Rejected {
                http_429: true,
                tx_fault: false,
                ..
            }
        ));
        assert!(matches!(
            classify_send(400, ""),
            SendOutcome::Rejected { tx_fault: true, .. }
        ));
        assert!(matches!(classify_send(503, ""), SendOutcome::Unknown(_)));
        assert!(matches!(classify_send(200, "x"), SendOutcome::Unknown(_)));
        let pre = r#"{"jsonrpc":"2.0","error":{"code":-32002,"message":"Transaction simulation failed: Error processing Instruction 0: custom program error: 0x177c","data":{"err":{"InstructionError":[0,{"Custom":6012}]}}},"id":1}"#;
        assert_eq!(
            classify_send(200, pre),
            SendOutcome::Rejected {
                reason: "Transaction simulation failed: Error processing Instruction 0: custom program error: 0x177c".into(),
                http_429: false,
                tx_fault: true,
                custom: Some((0, 6012))
            }
        );
        let node =
            r#"{"jsonrpc":"2.0","error":{"code":-32005,"message":"Node is unhealthy"},"id":1}"#;
        assert!(matches!(
            classify_send(200, node),
            SendOutcome::Rejected {
                tx_fault: false,
                ..
            }
        ));
        let dup = r#"{"jsonrpc":"2.0","error":{"code":-32002,"message":"This transaction has already been processed"},"id":1}"#;
        assert_eq!(classify_send(200, dup), SendOutcome::AlreadyProcessed);
        let b: Value = serde_json::from_str(&send_transaction_body(1, "AQ==")).unwrap();
        assert_eq!(b["params"][1]["skipPreflight"], false);
        assert_eq!(b["params"][1]["encoding"], "base64");
    }
}
