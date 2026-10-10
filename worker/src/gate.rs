//! Per-auction gate (one Durable Object per auction address): nonce replay protection +
//! submission rate limit (design §7.3 — a winner must not burn the Worker's fees with
//! endless bad submissions). The decision is a pure function (tested natively); the DO only
//! loads/stores the state. DO input gates keep `get → decide → put` atomic as long as there
//! is no non-storage `await` in between.

pub const WINDOW_SECS: i64 = 3_600;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GateState {
    /// (nonce, expires) — kept until expiry, then pruned. A plain array (not a map) so the
    /// serde-wasm-bindgen round-trip through DO storage is unambiguous.
    pub nonces: Vec<(String, i64)>,
    /// admitted submission times within the last WINDOW_SECS
    pub recent: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum GateError {
    Replay,
    RateLimited { retry_after_secs: i64 },
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
    if s.recent.len() >= max_per_window {
        let oldest = s.recent.iter().min().copied().unwrap_or(now);
        return Err(GateError::RateLimited {
            retry_after_secs: (oldest + WINDOW_SECS - now).max(1),
        });
    }
    s.nonces.push((nonce.to_string(), expires));
    s.recent.push(now);
    Ok(s)
}

#[cfg(target_arch = "wasm32")]
pub use object::AuctionGate;

#[cfg(target_arch = "wasm32")]
mod object {
    use super::*;
    use worker::*;

    const STATE_KEY: &str = "gate";

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
            if req.path() != "/admit" {
                return Response::error("not found", 404);
            }
            let r: AdmitReq = req.json().await?;
            // storage-only awaits from here: input gate keeps this atomic per auction
            let storage = self.state.storage();
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
                        GateError::Replay => 409,
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
