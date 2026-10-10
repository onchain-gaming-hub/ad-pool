//! Per-auction gate (one Durable Object per auction address): nonce replay protection +
//! submission rate limit (design §7.3 — a winner must not burn the Worker's fees with
//! endless bad submissions) + an in-flight lock (W2: one submission at a time per auction,
//! so two moderations never race to send conflicting txs). The decision is a pure function (tested natively); the DO only
//! loads/stores the state. DO input gates keep `get → decide → put` atomic as long as there
//! is no non-storage `await` in between.

pub const WINDOW_SECS: i64 = 3_600;
/// Lock lifetime: longer than one full submission (moderation + URL hops + R2 + confirm).
/// If the Worker dies mid-way the lock simply expires. If a lock ever expires while its
/// holder still runs, the re-read before the tx + the program's status checks still hold.
pub const LOCK_TTL_SECS: i64 = 180;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GateState {
    /// (nonce, expires) — kept until expiry, then pruned. A plain array (not a map) so the
    /// serde-wasm-bindgen round-trip through DO storage is unambiguous.
    pub nonces: Vec<(String, i64)>,
    /// admitted submission times within the last WINDOW_SECS
    pub recent: Vec<i64>,
    /// nonce of the submission currently being processed (empty = none) and its expiry.
    /// `serde(default)` keeps W1-era stored state readable.
    #[serde(default)]
    pub inflight_nonce: String,
    #[serde(default)]
    pub inflight_until: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum GateError {
    Replay,
    RateLimited {
        retry_after_secs: i64,
    },
    /// another submission for this auction is being processed — the nonce is NOT spent,
    /// the same signed request may be retried after `retry_after_secs`
    InFlight {
        retry_after_secs: i64,
    },
}

pub fn admit(
    mut s: GateState,
    nonce: &str,
    expires: i64,
    now: i64,
    max_per_window: usize,
) -> Result<GateState, GateError> {
    s.nonces.retain(|(_, exp)| *exp > now);
    s.recent.retain(|t| *t > now - WINDOW_SECS);
    if s.nonces.iter().any(|(n, _)| n == nonce) {
        return Err(GateError::Replay);
    }
    if s.inflight_until > now {
        return Err(GateError::InFlight {
            retry_after_secs: (s.inflight_until - now).max(1),
        });
    }
    if s.recent.len() >= max_per_window {
        let oldest = s.recent.iter().min().copied().unwrap_or(now);
        return Err(GateError::RateLimited {
            retry_after_secs: (oldest + WINDOW_SECS - now).max(1),
        });
    }
    s.nonces.push((nonce.to_string(), expires));
    s.recent.push(now);
    s.inflight_nonce = nonce.to_string();
    s.inflight_until = now + LOCK_TTL_SECS;
    Ok(s)
}

/// Releases the lock only if `nonce` holds it (a late release must not free a newer lock).
pub fn release(mut s: GateState, nonce: &str) -> GateState {
    if s.inflight_nonce == nonce {
        s.inflight_nonce.clear();
        s.inflight_until = 0;
    }
    s
}

#[cfg(target_arch = "wasm32")]
pub use object::AuctionGate;

#[cfg(target_arch = "wasm32")]
mod object {
    use super::*;
    use worker::*;

    const STATE_KEY: &str = "gate";

    #[derive(serde::Deserialize)]
    pub struct ReleaseReq {
        pub nonce: String,
    }

    #[derive(serde::Deserialize)]
    pub struct AdmitReq {
        pub nonce: String,
        pub expires: i64,
        pub now: i64,
        pub max_per_window: usize,
    }

    #[durable_object]
    pub struct AuctionGate {
        state: State,
    }

    impl DurableObject for AuctionGate {
        fn new(state: State, _env: Env) -> Self {
            Self { state }
        }

        async fn fetch(&self, mut req: Request) -> Result<Response> {
            let path = req.path();
            if path != "/admit" && path != "/release" {
                return Response::error("not found", 404);
            }
            let storage = self.state.storage();
            if path == "/release" {
                let r: ReleaseReq = req.json().await?;
                let current = match storage.get::<GateState>(STATE_KEY).await {
                    Ok(Some(s)) => s,
                    Ok(None) => return Response::from_json(&serde_json::json!({ "ok": true })),
                    Err(e) => {
                        console_error!("gate state unreadable: {e}");
                        return Response::error("gate_unavailable", 503);
                    }
                };
                storage.put(STATE_KEY, &release(current, &r.nonce)).await?;
                return Response::from_json(&serde_json::json!({ "ok": true }));
            }
            let r: AdmitReq = req.json().await?;
            // storage-only awaits from here: input gate keeps this atomic per auction
            // fail CLOSED: a read/deserialize error must not reset nonces + rate history
            let current: GateState = match storage.get::<GateState>(STATE_KEY).await {
                Ok(Some(s)) => s,
                Ok(None) => GateState::default(),
                Err(e) => {
                    console_error!("gate state unreadable: {e}");
                    return Response::error("gate_unavailable", 503);
                }
            };
            match admit(current, &r.nonce, r.expires, r.now, r.max_per_window) {
                Ok(next) => {
                    storage.put(STATE_KEY, &next).await?;
                    Response::from_json(&serde_json::json!({ "ok": true }))
                }
                Err(e) => {
                    let status = match e {
                        GateError::Replay | GateError::InFlight { .. } => 409,
                        GateError::RateLimited { .. } => 429,
                    };
                    Ok(
                        Response::from_json(&serde_json::json!({ "ok": false, "error": e }))?
                            .with_status(status),
                    )
                }
            }
        }
    }
}
