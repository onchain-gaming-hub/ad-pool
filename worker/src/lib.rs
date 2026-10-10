//! ad-pool moderator Worker (W0: scaffold, config, RPC, account decoding).
//! Plan: ../specs/designs/02-implementation-plan.md §4 · Design: ../specs/designs/01 §7.
//!
//! Routes (W0):
//! - `GET /health` — config + program IDs (no RPC)
//! - `GET /v1/health/rpc` — RPC reachable and on the expected cluster (genesis hash)
//! - `GET /v1/auction/:game_program/:epoch/:slot` — decoded `AdSlotAuction` + phase

pub mod accounts;
pub mod config;
pub mod pda;
pub mod phase;
pub mod rpc;

#[cfg(target_arch = "wasm32")]
mod app {
    use std::cell::RefCell;

    use serde_json::json;
    use solana_pubkey::Pubkey;
    use worker::*;

    use crate::{
        accounts, config, pda, phase,
        rpc::{RpcClient, RpcError, redact_url},
    };

    thread_local! {
        /// Genesis hash already verified by this isolate (a value, never a future —
        /// Workers forbid sharing I/O across requests).
        static CLUSTER_OK: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    fn load(env: &Env) -> Result<config::Config> {
        config::from_lookup(|n| env.var(n).ok().map(|v| v.to_string()))
            .map_err(|e| Error::RustError(e.0))
    }

    fn rpc(env: &Env, cfg: &config::Config) -> Result<RpcClient> {
        let url = env
            .secret(&config::rpc_secret_name(&cfg.cluster))?
            .to_string();
        config::validate_rpc_url(&url).map_err(|e| Error::RustError(e.0))?;
        Ok(RpcClient {
            url,
            timeout_ms: cfg.rpc_timeout_ms,
        })
    }

    /// RPC errors go to the client as generic codes only (the URL holds an API key);
    /// the log line carries the redacted host.
    fn rpc_fail(client: &RpcClient, e: &RpcError) -> Result<Response> {
        let detail = match e {
            RpcError::Http(_) => "transport error".to_string(),
            other => other.to_string(),
        };
        console_error!("rpc {} failed: {}", redact_url(&client.url), detail);
        err(502, e.public_code())
    }

    /// Verify the RPC is devnet once per isolate before any read that drives a decision.
    async fn ensure_cluster(
        client: &RpcClient,
        cfg: &config::Config,
    ) -> std::result::Result<(), RpcError> {
        if CLUSTER_OK.with(|c| c.borrow().as_deref() == Some(cfg.expected_genesis_hash.as_str())) {
            return Ok(());
        }
        client.ensure_cluster(&cfg.expected_genesis_hash).await?;
        CLUSTER_OK.with(|c| *c.borrow_mut() = Some(cfg.expected_genesis_hash.clone()));
        Ok(())
    }

    fn now() -> i64 {
        (Date::now().as_millis() / 1000) as i64
    }

    fn err(status: u16, msg: &str) -> Result<Response> {
        Ok(Response::from_json(&json!({ "error": msg }))?.with_status(status))
    }

    #[event(fetch)]
    async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
        Router::new()
            .get("/health", |_, ctx| {
                let cfg = load(&ctx.env)?;
                Response::from_json(&json!({
                    "ok": true,
                    "cluster": cfg.cluster,
                    "ad_pool": cfg.ad_pool.to_string(),
                    "platform_core": cfg.platform_core.to_string(),
                }))
            })
            // ⚠️ before W6 deploy: gate behind a token / edge rule (spends RPC quota)
            .get_async("/v1/health/rpc", |_, ctx| async move {
                let cfg = load(&ctx.env)?;
                let client = rpc(&ctx.env, &cfg)?;
                match ensure_cluster(&client, &cfg).await {
                    Ok(()) => Response::from_json(&json!({ "ok": true, "cluster": cfg.cluster })),
                    Err(e) => rpc_fail(&client, &e),
                }
            })
            // ⚠️ before W6 deploy: rate limit / cache (spends RPC quota)
            .get_async("/v1/auction/:game/:epoch/:slot", |_, ctx| async move {
                let cfg = load(&ctx.env)?;
                let game: Pubkey = match ctx.param("game").and_then(|g| g.parse().ok()) {
                    Some(g) => g,
                    None => return err(400, "bad game_program"),
                };
                let epoch: u64 = match ctx.param("epoch").and_then(|e| e.parse().ok()) {
                    Some(e) => e,
                    None => return err(400, "bad epoch"),
                };
                let slot: u8 = match ctx.param("slot").and_then(|s| s.parse().ok()) {
                    Some(s) => s,
                    None => return err(400, "bad slot"),
                };
                let client = rpc(&ctx.env, &cfg)?;
                if let Err(e) = ensure_cluster(&client, &cfg).await {
                    return rpc_fail(&client, &e);
                }
                let address = pda::ad_slot(&cfg.ad_pool, &game, epoch, slot);
                let info = match client.get_account_info(&address.to_string()).await {
                    Ok(Some(i)) => i,
                    Ok(None) => return err(404, "no auction"),
                    Err(e) => return rpc_fail(&client, &e),
                };
                match accounts::ad_slot_auction(&info, &cfg.ad_pool) {
                    Ok(a) => Response::from_json(&json!({
                        "address": address.to_string(),
                        "phase": phase::phase(now(), &a),
                        "auction": a,
                    })),
                    Err(e) => err(502, &format!("bad auction account: {e}")),
                }
            })
            .run(req, env)
            .await
    }
}
