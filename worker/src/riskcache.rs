//! Web Risk answer cache (plan W2 "cache ตาม `expireTime`" · W3): one Durable Object
//! (`RISK_CACHE`, name "webrisk") keyed by `webrisk::cache_key(uri)` (sha256 hex).
//! Lifetimes come from `webrisk::cache_until` (pure, tested). The cache is an optimization
//! only: if it fails, the Worker calls Web Risk directly — the risk check itself stays fail
//! closed (an unavailable Web Risk is never read as safe).

#[cfg(target_arch = "wasm32")]
pub use object::{RiskCache, lookup_cached};

#[cfg(target_arch = "wasm32")]
mod object {
    use serde_json::json;
    use worker::*;

    use crate::webrisk::{self, CacheEntry, RiskError};

    #[derive(serde::Deserialize)]
    struct GetReq {
        key: String,
        now: i64,
    }

    #[derive(serde::Deserialize)]
    struct PutReq {
        key: String,
        entry: CacheEntry,
    }

    #[durable_object]
    pub struct RiskCache {
        state: State,
    }

    impl DurableObject for RiskCache {
        fn new(state: State, _env: Env) -> Self {
            Self { state }
        }

        async fn fetch(&self, mut req: Request) -> Result<Response> {
            let storage = self.state.storage();
            match req.path().as_str() {
                "/get" => {
                    let r: GetReq = req.json().await?;
                    match storage.get::<CacheEntry>(&r.key).await? {
                        Some(e) if e.until > r.now => Response::from_json(&e),
                        Some(_) => {
                            storage.delete(&r.key).await?;
                            Response::error("miss", 404)
                        }
                        None => Response::error("miss", 404),
                    }
                }
                "/put" => {
                    let r: PutReq = req.json().await?;
                    storage.put(&r.key, &r.entry).await?;
                    Response::from_json(&json!({ "ok": true }))
                }
                _ => Response::error("not found", 404),
            }
        }
    }

    async fn call(
        stub: &Stub,
        path: &str,
        body: serde_json::Value,
        timeout_ms: u64,
    ) -> Option<Response> {
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_body(Some(body.to_string().into()));
        let req = Request::new_with_init(&format!("https://risk{path}"), &init).ok()?;
        let fut = Box::pin(stub.fetch_with_request(req));
        let timeout = Box::pin(Delay::from(core::time::Duration::from_millis(timeout_ms)));
        match futures_util::future::select(fut, timeout).await {
            futures_util::future::Either::Left((Ok(r), _)) => Some(r),
            _ => None,
        }
    }

    /// Threat types for `uri` (empty = no match), from the cache or Web Risk.
    pub async fn lookup_cached(
        env: &Env,
        api_key: &str,
        uri: &str,
        timeout_ms: u64,
        now: i64,
    ) -> std::result::Result<Vec<String>, RiskError> {
        let key = webrisk::cache_key(uri);
        let stub = env
            .durable_object("RISK_CACHE")
            .and_then(|ns| ns.id_from_name("webrisk")?.get_stub())
            .ok();
        if let Some(stub) = &stub
            && let Some(mut r) =
                call(stub, "/get", json!({ "key": key, "now": now }), timeout_ms).await
            && r.status_code() == 200
            && let Ok(e) = r.json::<CacheEntry>().await
        {
            console_log!("risk cache hit {}", &key[..12]);
            return Ok(e.threats);
        }
        let (threats, expire) = webrisk::lookup(api_key, uri, timeout_ms).await?;
        console_log!(
            "risk cache miss {} → web risk ({} threats)",
            &key[..12],
            threats.len()
        );
        if let Some(stub) = &stub {
            let entry = CacheEntry {
                until: webrisk::cache_until(now, &threats, expire),
                threats: threats.clone(),
            };
            if call(
                stub,
                "/put",
                json!({ "key": key, "entry": entry }),
                timeout_ms,
            )
            .await
            .is_none()
            {
                console_error!("risk cache put failed (continuing)");
            }
        }
        Ok(threats)
    }
}
