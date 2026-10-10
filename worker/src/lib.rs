//! ad-pool moderator Worker (W0: scaffold, config, RPC, decoding · W1: creative intake).
//! Plan: ../specs/designs/02-implementation-plan.md §4 · Design: ../specs/designs/01 §7.
//!
//! Routes (W0):
//! - `GET /health` — config + program IDs (no RPC)
//! - `GET /v1/health/rpc` — RPC reachable and on the expected cluster (genesis hash)
//! - `GET /v1/auction/:game_program/:epoch/:slot` — decoded `AdSlotAuction` + phase
//! - `POST /v1/creative` — multipart submission from the winning bidder (W1: auth + checks;
//!   moderation, R2 write and the on-chain approve come in W2). API: ../specs/designs/04-worker-api-v1.md

pub mod accounts;
pub mod auth;
pub mod config;
pub mod gate;
pub mod image;
pub mod pda;
pub mod phase;
pub mod rpc;
pub mod submission;

#[cfg(target_arch = "wasm32")]
mod app {
    use std::cell::RefCell;

    use serde_json::json;
    use solana_pubkey::Pubkey;
    use worker::*;

    use crate::{
        accounts, auth, config, pda, phase,
        rpc::{RpcClient, RpcError, redact_url},
        submission::{self, Input, Limits},
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

    fn reject(r: &submission::Reject) -> Result<Response> {
        Ok(
            Response::from_json(&json!({ "error": "rejected", "reason": r }))?
                .with_status(r.status()),
        )
    }

    fn field(form: &FormData, name: &str) -> Option<String> {
        match form.get(name) {
            Some(FormEntry::Field(v)) => Some(v),
            _ => None,
        }
    }

    async fn file(form: &FormData, name: &str) -> Result<Option<Vec<u8>>> {
        match form.get(name) {
            Some(FormEntry::File(f)) => Ok(Some(f.bytes().await?)),
            _ => Ok(None),
        }
    }

    /// W1 intake. Order: cheap local checks + signature → cluster guard → owner-checked
    /// auction read → chain checks → per-auction gate (nonce + rate limit).
    /// ⚠️ before W6: per-IP/per-signer limit in front of the RPC read (any keypair can sign
    /// its own message and cost one getAccountInfo) · W2: in-flight lock in the gate DO.
    /// The nonce is spent at admission — a later transient failure means re-signing.
    async fn post_creative(mut req: Request, env: &Env) -> Result<Response> {
        let cfg = load(env)?;
        // refuse oversized bodies before reading them (multipart overhead allowance 64 KiB)
        let declared: Option<usize> = req
            .headers()
            .get("content-length")?
            .and_then(|v| v.parse().ok());
        match declared {
            Some(n) if n <= cfg.max_image_bytes + 64 * 1024 => {}
            Some(_) => return err(413, "too_large"),
            None => return err(411, "length_required"),
        }
        let form = match req.form_data().await {
            Ok(f) => f,
            Err(_) => return err(400, "multipart_required"),
        };
        let input = Input {
            game: field(&form, "game").unwrap_or_default(),
            epoch: field(&form, "epoch").unwrap_or_default(),
            slot: field(&form, "slot").unwrap_or_default(),
            signer: field(&form, "signer").unwrap_or_default(),
            nonce: field(&form, "nonce").unwrap_or_default(),
            expires: field(&form, "expires").unwrap_or_default(),
            signature: field(&form, "signature").unwrap_or_default(),
            image: file(&form, "image").await?,
            text: field(&form, "text"),
            click_url: field(&form, "click_url"),
        };
        let limits = Limits {
            domain: &cfg.auth_domain,
            cluster: &cfg.cluster,
            ad_pool: &cfg.ad_pool,
            max_image_bytes: cfg.max_image_bytes,
        };
        let t = now();
        let prepared = match submission::prevalidate(input, &limits, t) {
            Ok(p) => p,
            Err(r) => return reject(&r),
        };

        let client = rpc(env, &cfg)?;
        if let Err(e) = ensure_cluster(&client, &cfg).await {
            return rpc_fail(&client, &e);
        }
        let info = match client.get_account_info(&prepared.auction.to_string()).await {
            Ok(Some(i)) => i,
            Ok(None) => return err(404, "no_auction"),
            Err(e) => return rpc_fail(&client, &e),
        };
        let auction = match accounts::ad_slot_auction(&info, &cfg.ad_pool) {
            Ok(a) => a,
            Err(e) => {
                console_error!("auction {} undecodable: {e}", prepared.auction);
                return err(502, "bad_auction_account");
            }
        };
        if let Err(r) = submission::against_auction(&prepared, &auction, t) {
            return reject(&r);
        }

        // nonce + rate limit, serialized per auction
        let gate = env
            .durable_object("AUCTION_GATE")?
            .id_from_name(&prepared.auction.to_string())?
            .get_stub()?;
        let body = json!({
            "nonce": prepared.message.nonce,
            "expires": prepared.message.expires,
            "now": t,
            "max_per_window": cfg.max_submissions_per_hour,
        });
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_body(Some(body.to_string().into()));
        // race the DO call like every fetch (gotcha 15)
        let call =
            Box::pin(gate.fetch_with_request(Request::new_with_init("https://gate/admit", &init)?));
        let timeout = Box::pin(Delay::from(core::time::Duration::from_millis(
            cfg.rpc_timeout_ms,
        )));
        let mut gate_resp = match futures_util::future::select(call, timeout).await {
            futures_util::future::Either::Left((r, _)) => r?,
            futures_util::future::Either::Right(_) => return err(503, "gate_unavailable"),
        };
        match gate_resp.status_code() {
            200 => {}
            code @ (409 | 429) => {
                let detail: serde_json::Value = gate_resp.json().await.unwrap_or(json!({}));
                return Ok(Response::from_json(
                    &json!({ "error": "gate", "reason": detail["error"] }),
                )?
                .with_status(code));
            }
            code => {
                console_error!("auction gate returned {code}");
                return err(503, "gate_unavailable");
            }
        }

        // W2 continues here: moderation → R2 → approve/reject tx.
        Response::from_json(&json!({
            "status": "pending_moderation",
            "kind": prepared.kind,
            "auction": prepared.auction.to_string(),
            "image_sha256": auth::hex(&prepared.image_sha256),
            "image_type": prepared.image_info.kind,
            "payload": auth::hex(&prepared.message.payload),
        }))
        .map(|r| r.with_status(202))
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
            .post_async("/v1/creative", |req, ctx| async move {
                post_creative(req, &ctx.env).await
            })
            .run(req, env)
            .await
    }
}
