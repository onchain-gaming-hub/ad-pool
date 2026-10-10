//! ad-pool moderator Worker (W0: scaffold, config, RPC, decoding · W1: creative intake ·
//! W2: moderation, click_url layers 1–2, R2, on-chain approve/reject).
//! Plan: ../specs/designs/02-implementation-plan.md §4 · Design: ../specs/designs/01 §7.
//!
//! Routes (W0):
//! - `GET /health` — config + program IDs (no RPC)
//! - `GET /v1/health/rpc` — RPC reachable and on the expected cluster (genesis hash)
//! - `GET /v1/auction/:game_program/:epoch/:slot` — decoded `AdSlotAuction` + phase
//! - `POST /v1/creative` — multipart submission from the winning bidder: auth + checks (W1)
//!   → moderation + URL checks → R2 → approve/reject tx (W2). API: ../specs/designs/04-worker-api-v1.md
//! - `GET /ads/<sha256>.<ext>` — approved image from R2 (content type from the checked kind)
//! - Cron (W3, `crons` in wrangler.toml): settle / close_auction every due auction (crank key)
//!   + re-check click_url of live creatives → `revoke_creative` (Moderator key)

pub mod accounts;
pub mod ads;
pub mod auth;
pub mod config;
pub mod crank;
pub mod decision;
pub mod gate;
pub mod image;
pub mod moderation;
pub mod pda;
pub mod phase;
pub mod riskcache;
pub mod rpc;
pub mod submission;
pub mod tx;
pub mod urlcheck;
pub mod webrisk;

#[cfg(target_arch = "wasm32")]
mod app {
    use std::cell::RefCell;

    use serde_json::json;
    use solana_pubkey::Pubkey;
    use worker::*;

    use crate::{
        accounts, ads, auth, config, crank,
        decision::{self, Check, Outcome, Parts},
        moderation, pda, phase,
        rpc::{RpcClient, RpcError, SendOutcome, TxStatus, redact_url},
        submission::{self, Input, Kind, Limits, Prepared},
        tx::{self, Action, flags},
        urlcheck::{self, Hop},
    };
    use solana_signer::Signer;

    /// Confirmation polling after `sendTransaction` (1 s apart).
    const CONFIRM_POLLS: u32 = 20;
    /// Per-hop timeout when following a click_url's redirects.
    const URL_FETCH_TIMEOUT_MS: u64 = 5_000;
    const CONFIRM_POLL_TIMEOUT_MS: u64 = 2_000;

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

    /// Order: cheap local checks + signature + URL layer 1 → cluster guard → owner-checked
    /// auction read → chain checks → per-auction gate (nonce + rate limit + in-flight lock)
    /// → `moderate_and_act` (W2) → lock released.
    /// ⚠️ before W6: per-IP/per-signer limit in front of the RPC read (any keypair can sign
    /// its own message and cost one getAccountInfo).
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

        // nonce + rate limit + in-flight lock, serialized per auction
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
        let release = json!({ "nonce": prepared.message.nonce });
        let mut gate_resp = match gate_call(&gate, "admit", body, cfg.rpc_timeout_ms).await {
            Some(r) => r,
            None => {
                // the DO may have stored the admit before we timed out: free a lock we may hold
                let _ = gate_call(&gate, "release", release, cfg.rpc_timeout_ms).await;
                return err(503, "gate_unavailable");
            }
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

        // the lock is held from here: release on every path — except while a sent tx is
        // still unconfirmed (a resubmission could then land a second tx); it expires itself
        let (out, hold) = moderate_and_act(env, &cfg, &client, &prepared).await;
        if hold {
            console_log!("{}: tx in flight, lock kept until TTL", prepared.auction);
            return out;
        }
        if gate_call(&gate, "release", release, cfg.rpc_timeout_ms)
            .await
            .is_none_or(|r| r.status_code() != 200)
        {
            console_error!(
                "gate release failed for {} (lock expires on its own)",
                prepared.auction
            );
        }
        out
    }

    /// One call to the per-auction DO, raced against a timeout (gotcha 15). `None` = failed.
    async fn gate_call(
        stub: &Stub,
        path: &str,
        body: serde_json::Value,
        timeout_ms: u64,
    ) -> Option<Response> {
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_body(Some(body.to_string().into()));
        let req = Request::new_with_init(&format!("https://gate/{path}"), &init).ok()?;
        let call = Box::pin(stub.fetch_with_request(req));
        let timeout = Box::pin(Delay::from(core::time::Duration::from_millis(timeout_ms)));
        match futures_util::future::select(call, timeout).await {
            futures_util::future::Either::Left((Ok(r), _)) => Some(r),
            futures_util::future::Either::Left((Err(e), _)) => {
                console_error!("auction gate {path} failed: {e}");
                None
            }
            futures_util::future::Either::Right(_) => None,
        }
    }

    fn secret_or_empty(env: &Env, name: &str) -> String {
        env.secret(name).map(|s| s.to_string()).unwrap_or_default()
    }

    fn moderation_check(
        r: std::result::Result<moderation::Verdict, moderation::ModError>,
        what: &str,
        flag: u32,
        auction: &Pubkey,
    ) -> Check {
        match r {
            Ok(v) => {
                console_log!(
                    "moderation {what} {auction}: flagged={} categories={:?} scores={:?}",
                    v.flagged,
                    v.categories,
                    v.scores
                );
                if v.flagged {
                    Check::fail(flag, format!("{what}:{}", v.categories.join(",")))
                } else {
                    Check::Pass
                }
            }
            Err(e) => {
                console_error!("moderation {what} {auction} unavailable: {}", e.code());
                Check::Unavailable(format!("openai_{what}:{}", e.code()))
            }
        }
    }

    /// Layers 1 + 2 on the submitted (normalized) URL and every redirect hop. Web Risk runs
    /// before each fetch, so a known-bad host is never contacted.
    async fn url_chain(env: &Env, start: &str, web_risk_key: &str, timeout_ms: u64) -> Check {
        let Ok(mut current) = urlcheck::layer1(start) else {
            return Check::fail(flags::URL_LAYER1, "layer1");
        };
        for _ in 0..=urlcheck::MAX_REDIRECTS {
            match crate::riskcache::lookup_cached(
                env,
                web_risk_key,
                current.as_str(),
                timeout_ms,
                now(),
            )
            .await
            {
                Ok(t) if t.is_empty() => {}
                Ok(t) => {
                    console_log!(
                        "web risk match {}: {:?}",
                        current.host_str().unwrap_or(""),
                        t
                    );
                    return Check::fail(flags::URL_LAYER2, format!("web_risk:{}", t.join(",")));
                }
                Err(e) => {
                    console_error!("web risk unavailable: {}", e.code());
                    return Check::Unavailable(format!("web_risk:{}", e.code()));
                }
            }
            let mut init = RequestInit::new();
            init.with_method(Method::Get)
                .with_redirect(RequestRedirect::Manual);
            let Ok(req) = Request::new_with_init(current.as_str(), &init) else {
                // the URL already passed layer 1: this is a Worker fault, not the advertiser's
                return Check::Unavailable("click_url_request".into());
            };
            let fetch = Box::pin(async move { Fetch::Request(req).send().await });
            let timeout = Box::pin(Delay::from(core::time::Duration::from_millis(
                URL_FETCH_TIMEOUT_MS,
            )));
            let resp = match futures_util::future::select(fetch, timeout).await {
                futures_util::future::Either::Left((Ok(r), _)) => r,
                // cannot see where it leads → cannot approve (fail closed, no reject)
                _ => return Check::Unavailable("click_url_unreachable".into()),
            };
            let location = resp.headers().get("location").ok().flatten();
            match urlcheck::next_hop(&current, resp.status_code(), location.as_deref()) {
                Ok(Hop::Final) => return Check::Pass,
                Ok(Hop::Redirect(next)) => current = next,
                Err(r) => return Check::fail(flags::URL_LAYER1, format!("redirect:{r:?}")),
            }
        }
        Check::fail(flags::URL_LAYER1, "redirect:TooManyRedirects")
    }

    fn details(checks: &[&Check]) -> Vec<String> {
        checks
            .iter()
            .filter_map(|c| match c {
                Check::Fail { detail, .. } => Some(detail.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// W2: moderation + URL layers → decision → R2 → re-read → tx. Runs under the lock.
    async fn moderate_and_act(
        env: &Env,
        cfg: &config::Config,
        client: &RpcClient,
        p: &Prepared,
    ) -> (Result<Response>, bool) {
        // the signing key first — no paid API call if the Worker cannot act anyway
        let moderator = match tx::keypair_from_base58(&secret_or_empty(env, "MODERATOR_KEYPAIR")) {
            Ok(k) if k.pubkey() == cfg.moderator => k,
            Ok(_) => {
                console_error!("MODERATOR_KEYPAIR does not hold MODERATOR_ADDRESS");
                return done(err(503, "moderator_unavailable"));
            }
            Err(e) => {
                console_error!("moderator key: {e}");
                return done(err(503, "moderator_unavailable"));
            }
        };
        let openai = secret_or_empty(env, "OPENAI_API_KEY");
        let image_call = moderation::call(
            &openai,
            moderation::image_body(p.image_info.kind, &p.image),
            cfg.rpc_timeout_ms,
        );
        let (image, text, url) = if p.kind == Kind::Full {
            let web_risk = secret_or_empty(env, "WEB_RISK_API_KEY");
            let (i, t, u) = futures_util::future::join3(
                image_call,
                moderation::call(&openai, moderation::text_body(&p.text), cfg.rpc_timeout_ms),
                url_chain(env, &p.click_url_normalized, &web_risk, cfg.rpc_timeout_ms),
            )
            .await;
            (
                moderation_check(i, "image", flags::IMAGE_MODERATION, &p.auction),
                moderation_check(t, "text", flags::TEXT_MODERATION, &p.auction),
                u,
            )
        } else {
            let i = image_call.await;
            (
                moderation_check(i, "image", flags::IMAGE_MODERATION, &p.auction),
                Check::Pass,
                Check::Pass,
            )
        };
        let parts = Parts {
            image_url: ads::image_url(&cfg.public_base_url, &p.image_sha256, p.image_info.kind),
            image_sha256: p.image_sha256,
            text: p.text.clone(),
            click_url: p.click_url_normalized.clone(),
        };
        let action = match decision::decide(p.kind, &image, &text, &url, &parts) {
            Outcome::Act(a) => a,
            Outcome::Retry { unavailable } => {
                console_error!("{}: no decision, unavailable {:?}", p.auction, unavailable);
                return done(json_status(
                    json!({ "error": "moderation_unavailable", "unavailable": unavailable }),
                    503,
                ));
            }
        };

        // R2 before the chain: once the tx lands, the URL must already serve the image (A1)
        if matches!(
            action,
            Action::ApproveFull { .. } | Action::ApproveImage { .. }
        ) {
            let key = ads::object_key(&p.image_sha256, p.image_info.kind);
            let bucket = match env.bucket("ADS") {
                Ok(b) => b,
                Err(e) => return done(Err(e)),
            };
            let put = bucket
                .put(&key, p.image.clone())
                .http_metadata(HttpMetadata {
                    content_type: Some(p.image_info.kind.mime().to_string()),
                    ..Default::default()
                })
                .execute()
                .await;
            if let Err(e) = put {
                console_error!("r2 put {key} failed: {e}");
                return done(err(503, "storage_unavailable"));
            }
        }

        // re-read: the auction may have changed while moderation ran
        let fresh = match client.get_account_info(&p.auction.to_string()).await {
            Ok(Some(i)) => i,
            Ok(None) => return done(err(404, "no_auction")),
            Err(e) => return done(rpc_fail(client, &e)),
        };
        let a = match accounts::ad_slot_auction(&fresh, &cfg.ad_pool) {
            Ok(a) => a,
            Err(e) => {
                console_error!("auction {} undecodable: {e}", p.auction);
                return done(err(502, "bad_auction_account"));
            }
        };
        if let Err(r) = submission::against_auction(p, &a, now()) {
            return done(reject(&r));
        }

        let (blockhash, _) = match client.get_latest_blockhash().await {
            Ok(b) => b,
            Err(e) => return done(rpc_fail(client, &e)),
        };
        let ix = tx::instruction(
            &cfg.ad_pool,
            &cfg.platform_core,
            &moderator.pubkey(),
            &p.auction,
            &action,
        );
        let (wire, signature) = match tx::sign(&moderator, ix, &blockhash) {
            Ok(x) => x,
            Err(e) => {
                console_error!("sign: {e}");
                return done(err(500, "sign_failed"));
            }
        };
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&wire);
        let label = match &action {
            Action::ApproveFull { .. } => "approved",
            Action::ApproveTextOnly { .. } => "text_only",
            Action::ApproveImage { .. } => "image_approved",
            Action::Reject { .. } => "rejected",
            // never produced by `decide` (W3 crank only)
            Action::Revoke { .. } => "revoked",
        };
        console_log!("{} {:?} → {label} sig {signature}", p.auction, p.kind);
        match client.send_transaction(&b64).await {
            SendOutcome::Accepted(_) | SendOutcome::AlreadyProcessed => {}
            SendOutcome::Rejected {
                reason,
                http_429,
                tx_fault,
                custom,
            } => {
                console_error!(
                    "send rejected ({}): {reason} custom={custom:?}",
                    redact_url(&client.url)
                );
                if http_429 {
                    return done(err(503, "rpc_rate_limited"));
                }
                if !tx_fault {
                    // node-side (unhealthy / internal): retryable, the tx was not judged
                    return done(err(503, "rpc_unavailable"));
                }
                return done(json_status(
                    json!({ "error": "tx_rejected", "program_error": custom.map(|(_, c)| c) }),
                    502,
                ));
            }
            SendOutcome::Unknown(why) => {
                console_error!("send unknown: {why}");
                // may have landed → keep the lock
                return (
                    json_status(
                        json!({ "error": "tx_unknown", "signature": signature }),
                        502,
                    ),
                    true,
                );
            }
        }
        let mut status = TxStatus::Pending;
        // short per-poll timeout keeps the whole wait well inside the gate lock TTL
        let poller = RpcClient {
            url: client.url.clone(),
            timeout_ms: CONFIRM_POLL_TIMEOUT_MS,
        };
        for _ in 0..CONFIRM_POLLS {
            Delay::from(core::time::Duration::from_millis(1_000)).await;
            match poller.get_signature_status(&signature).await {
                Ok(TxStatus::Pending) | Err(_) => continue,
                Ok(s) => {
                    status = s;
                    break;
                }
            }
        }
        let mut body = json!({
            "status": label,
            "signature": signature,
            "auction": p.auction.to_string(),
            "image_sha256": auth::hex(&p.image_sha256),
        });
        match &action {
            Action::ApproveFull { image_url, .. } | Action::ApproveImage { image_url, .. } => {
                body["image_url"] = json!(image_url);
            }
            Action::ApproveTextOnly {
                image_reject_flags, ..
            } => {
                body["image_reject_flags"] = json!(image_reject_flags);
                body["reasons"] = json!(details(&[&image]));
            }
            Action::Reject { flags } | Action::Revoke { flags } => {
                body["flags"] = json!(flags);
                body["reasons"] = json!(details(&[&image, &text, &url]));
            }
        }
        match status {
            TxStatus::Confirmed => {
                let code = if matches!(action, Action::Reject { .. }) {
                    422
                } else {
                    200
                };
                done(json_status(body, code))
            }
            TxStatus::Failed { custom, err: e } => {
                console_error!("tx {signature} failed: {e}");
                done(json_status(
                    json!({
                        "error": "tx_failed", "signature": signature,
                        "program_error": custom.map(|(_, c)| c),
                    }),
                    502,
                ))
            }
            TxStatus::Pending => {
                body["confirmed"] = json!(false);
                (json_status(body, 202), true)
            }
        }
    }

    fn json_status(v: serde_json::Value, status: u16) -> Result<Response> {
        Ok(Response::from_json(&v)?.with_status(status))
    }

    /// `moderate_and_act` result with the lock released afterwards.
    fn done(r: Result<Response>) -> (Result<Response>, bool) {
        (r, false)
    }

    async fn get_ad(file: &str, if_none_match: Option<String>, env: &Env) -> Result<Response> {
        let Some((key, kind)) = ads::parse_file(file) else {
            return err(404, "not_found");
        };
        // content-addressed: a matching ETag can never be stale → no R2 read, no hashing
        if if_none_match.as_deref() == Some(format!("\"{}\"", &file[..64]).as_str()) {
            return Ok(Response::empty()?.with_status(304));
        }
        let Some(obj) = env.bucket("ADS")?.get(&key).execute().await? else {
            return err(404, "not_found");
        };
        let Some(body) = obj.body() else {
            return err(404, "not_found");
        };
        let bytes = body.bytes().await?;
        // the key is the content hash — refuse to serve anything else under it
        let sha_hex = &file[..64];
        if auth::hex(&auth::sha256(&bytes)) != sha_hex {
            console_error!("r2 object {key} does not match its hash");
            return err(500, "integrity");
        }
        let headers = Headers::new();
        for (k, v) in ads::serve_headers(kind, sha_hex) {
            headers.set(k, &v)?;
        }
        Ok(Response::from_bytes(bytes)?.with_headers(headers))
    }

    /// Loads a keypair secret and checks it holds the configured address.
    fn signer(env: &Env, secret: &str, want: &Pubkey) -> Option<solana_keypair::Keypair> {
        match tx::keypair_from_base58(&secret_or_empty(env, secret)) {
            Ok(k) if k.pubkey() == *want => Some(k),
            Ok(_) => {
                console_error!("{secret} does not hold its configured address");
                None
            }
            Err(e) => {
                console_error!("{secret}: {e}");
                None
            }
        }
    }

    /// Sign + send one tx; logs the outcome (the next run sees the result on chain).
    async fn send_logged(
        client: &RpcClient,
        payer: &solana_keypair::Keypair,
        ixs: &[solana_instruction::Instruction],
        what: &str,
    ) -> bool {
        let (blockhash, _) = match client.get_latest_blockhash().await {
            Ok(b) => b,
            Err(e) => {
                console_error!("crank {what}: blockhash {}", e.public_code());
                return false;
            }
        };
        let (wire, sig) = match tx::sign_many(payer, ixs, &blockhash) {
            Ok(x) => x,
            Err(e) => {
                console_error!("crank {what}: sign {e}");
                return false;
            }
        };
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&wire);
        match client.send_transaction(&b64).await {
            SendOutcome::Accepted(_) | SendOutcome::AlreadyProcessed => {
                console_log!("crank {what}: sent {sig}");
                true
            }
            SendOutcome::Rejected { reason, custom, .. } => {
                console_error!("crank {what}: rejected {reason} custom={custom:?}");
                false
            }
            SendOutcome::Unknown(why) => {
                console_error!("crank {what}: unknown {why} sig {sig}");
                false
            }
        }
    }

    /// W3 crank run (design §6.1 + §7.4). Never panics; every failure is logged and the
    /// next run retries.
    async fn run_crank(env: &Env) -> Result<()> {
        let cfg = load(env)?;
        let client = rpc(env, &cfg)?;
        if let Err(e) = ensure_cluster(&client, &cfg).await {
            console_error!("crank: rpc {}", e.public_code());
            return Ok(());
        }
        let t = now();
        let list = match client
            .get_program_accounts(
                &cfg.ad_pool.to_string(),
                accounts::AD_SLOT_AUCTION_SIZE,
                &accounts::AD_SLOT_AUCTION_DISCRIMINATOR,
            )
            .await
        {
            Ok(l) => l,
            Err(e) => {
                console_error!("crank: getProgramAccounts {}", e.public_code());
                return Ok(());
            }
        };
        let mut auctions = Vec::with_capacity(list.len());
        for (k, info) in list {
            match accounts::ad_slot_auction(&info, &cfg.ad_pool) {
                Ok(a) => auctions.push((k, a)),
                Err(e) => console_error!("crank: auction {k} undecodable: {e}"),
            }
        }
        let plan = crank::plan(t, &auctions);
        for s in &plan.stale {
            console_error!(
                "STALE {} {:?} overdue {} s",
                s.auction,
                s.what,
                s.overdue_secs
            );
        }
        let find = |k: &Pubkey| auctions.iter().find(|(x, _)| x == k).map(|(_, a)| a);

        let mut sent = 0;
        if !plan.txs.is_empty()
            && let Some(crank_key) = signer(env, "CRANK_KEYPAIR", &cfg.crank)
        {
            for (k, due) in &plan.txs {
                let Some(a) = find(k) else { continue };
                let mut ixs = vec![];
                if matches!(due, crank::Due::Settle | crank::Due::SettleAndClose) {
                    // GameEntry.owner is read at settle time (pc rev 3 #6)
                    let entry = pda::game_entry(&cfg.platform_core, &a.game_program);
                    let owner = match client.get_account_info(&entry.to_string()).await {
                        Ok(Some(info)) => match accounts::game_entry(&info, &cfg.platform_core) {
                            Ok(g) => g.owner,
                            Err(e) => {
                                console_error!("crank {k}: game entry undecodable: {e}");
                                continue;
                            }
                        },
                        Ok(None) => {
                            console_error!("crank {k}: no GameEntry — settle impossible");
                            continue;
                        }
                        Err(e) => {
                            console_error!("crank {k}: game entry {}", e.public_code());
                            continue;
                        }
                    };
                    ixs.push(tx::settle_instruction(
                        &cfg.ad_pool,
                        &cfg.platform_core,
                        k,
                        &a.game_program,
                        &owner,
                    ));
                }
                if matches!(due, crank::Due::Close | crank::Due::SettleAndClose) {
                    ixs.push(tx::close_instruction(&cfg.ad_pool, k, &a.rent_payer));
                }
                if send_logged(&client, &crank_key, &ixs, &format!("{due:?} {k}")).await {
                    sent += 1;
                }
            }
        }

        let mut revoked = 0;
        let mut skipped = 0;
        if !plan.rechecks.is_empty() {
            let web_risk = secret_or_empty(env, "WEB_RISK_API_KEY");
            let moderator = signer(env, "MODERATOR_KEYPAIR", &cfg.moderator);
            for k in &plan.rechecks {
                let Some(a) = find(k) else { continue };
                match url_chain(env, &a.creative.click_url, &web_risk, cfg.rpc_timeout_ms).await {
                    Check::Pass => {}
                    Check::Unavailable(r) => {
                        console_error!("recheck {k}: unavailable {r} (no action)")
                    }
                    Check::Fail { flags, detail } => {
                        console_error!("recheck {k}: click_url failed {detail:?} → revoke");
                        let Some(m) = &moderator else {
                            console_error!("recheck {k}: revoke SKIPPED — no usable moderator key");
                            skipped += 1;
                            continue;
                        };
                        let ix = tx::instruction(
                            &cfg.ad_pool,
                            &cfg.platform_core,
                            &m.pubkey(),
                            k,
                            &Action::Revoke { flags: flags.get() },
                        );
                        if send_logged(&client, m, &[ix], &format!("revoke {k}")).await {
                            revoked += 1;
                        }
                    }
                }
            }
        }
        console_log!(
            "crank run: {} auctions · {} due ({} sent, {} deferred) · {} rechecked · {} revoked · {} revoke skipped · {} stale",
            auctions.len(),
            plan.txs.len() + plan.deferred,
            sent,
            plan.deferred,
            plan.rechecks.len(),
            revoked,
            skipped,
            plan.stale.len()
        );
        Ok(())
    }

    #[event(scheduled)]
    async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
        if let Err(e) = run_crank(&env).await {
            console_error!("crank run failed: {e}");
        }
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
            .get_async("/ads/:file", |req, ctx| async move {
                let file = ctx.param("file").cloned().unwrap_or_default();
                let inm = req.headers().get("if-none-match").ok().flatten();
                get_ad(&file, inm, &ctx.env).await
            })
            .post_async("/v1/creative", |req, ctx| async move {
                post_creative(req, &ctx.env).await
            })
            .run(req, env)
            .await
    }
}
