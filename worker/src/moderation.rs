//! OpenAI moderation (A9 · W2): `POST /v1/moderations` with `omni-moderation-latest`.
//! Image and text are **two separate calls** (plan W2) so each verdict maps to its own reason
//! bit. Request bodies and response parsing are pure (tested natively); the HTTP call is Wasm.
//! Verified live 2026-10-10 (image data URL + Thai text): `results[0]` carries `flagged`,
//! `categories`, `category_scores`, `category_applied_input_types`.
//! Policy v1: the verdict is OpenAI's own `flagged`. Any failure (HTTP, timeout, shape) is
//! `Unavailable` → the Worker never approves on it (fail closed) and sends no reject either.

use base64::Engine;
use serde_json::{Value, json};

use crate::image::ImageKind;

pub const MODERATIONS_URL: &str = "https://api.openai.com/v1/moderations";
/// Always named explicitly (plan W2) — never rely on the API default.
pub const MODEL: &str = "omni-moderation-latest";

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Verdict {
    pub flagged: bool,
    /// categories OpenAI marked true
    pub categories: Vec<String>,
    /// every category score, highest first (logged on every call — plan W2)
    pub scores: Vec<(String, f64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModError {
    /// HTTP status ≠ 200 (429 rate limit, 401 bad key, 5xx …)
    Http(u16),
    Transport,
    Timeout,
    BadResponse(&'static str),
    MissingKey,
}

impl ModError {
    pub fn code(&self) -> String {
        match self {
            ModError::Http(s) => format!("http_{s}"),
            ModError::Transport => "transport".into(),
            ModError::Timeout => "timeout".into(),
            ModError::BadResponse(w) => format!("bad_response:{w}"),
            ModError::MissingKey => "missing_key".into(),
        }
    }
}

pub fn image_body(kind: ImageKind, bytes: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    json!({
        "model": MODEL,
        "input": [{
            "type": "image_url",
            "image_url": { "url": format!("data:{};base64,{b64}", kind.mime()) }
        }]
    })
    .to_string()
}

pub fn text_body(text: &str) -> String {
    json!({ "model": MODEL, "input": text }).to_string()
}

pub fn parse(response: &str) -> Result<Verdict, ModError> {
    let v: Value = serde_json::from_str(response).map_err(|_| ModError::BadResponse("json"))?;
    let r = v
        .get("results")
        .and_then(Value::as_array)
        .and_then(|a| (a.len() == 1).then(|| &a[0]))
        .ok_or(ModError::BadResponse("results"))?;
    let flagged = r
        .get("flagged")
        .and_then(Value::as_bool)
        .ok_or(ModError::BadResponse("flagged"))?;
    let cats = r
        .get("categories")
        .and_then(Value::as_object)
        .ok_or(ModError::BadResponse("categories"))?;
    let mut categories: Vec<String> = cats
        .iter()
        .filter(|(_, v)| v.as_bool() == Some(true))
        .map(|(k, _)| k.clone())
        .collect();
    categories.sort();
    let mut scores: Vec<(String, f64)> = r
        .get("category_scores")
        .and_then(Value::as_object)
        .ok_or(ModError::BadResponse("category_scores"))?
        .iter()
        .filter_map(|(k, v)| v.as_f64().map(|s| (k.clone(), s)))
        .collect();
    scores.sort_by(|a, b| b.1.total_cmp(&a.1));
    // a flagged result must name a category, and a named category means flagged
    if flagged == categories.is_empty() {
        return Err(ModError::BadResponse("flagged/categories disagree"));
    }
    Ok(Verdict {
        flagged,
        categories,
        scores,
    })
}

#[cfg(target_arch = "wasm32")]
pub async fn call(api_key: &str, body: String, timeout_ms: u64) -> Result<Verdict, ModError> {
    use futures_util::future::{Either, select};
    use worker::{Delay, Fetch, Headers, Method, Request, RequestInit};

    if api_key.is_empty() {
        return Err(ModError::MissingKey);
    }
    let headers = Headers::new();
    headers
        .set("content-type", "application/json")
        .map_err(|_| ModError::Transport)?;
    headers
        .set("authorization", &format!("Bearer {api_key}"))
        .map_err(|_| ModError::Transport)?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(body.into()));
    let req = Request::new_with_init(MODERATIONS_URL, &init).map_err(|_| ModError::Transport)?;
    let fetch = Box::pin(async move {
        let mut resp = Fetch::Request(req)
            .send()
            .await
            .map_err(|_| ModError::Transport)?;
        if resp.status_code() != 200 {
            return Err(ModError::Http(resp.status_code()));
        }
        resp.text().await.map_err(|_| ModError::Transport)
    });
    let timeout = Box::pin(Delay::from(core::time::Duration::from_millis(timeout_ms)));
    match select(fetch, timeout).await {
        Either::Left((r, _)) => parse(&r?),
        Either::Right(_) => Err(ModError::Timeout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_name_the_model_and_split_inputs() {
        let i: Value = serde_json::from_str(&image_body(ImageKind::Png, &[1, 2, 3])).unwrap();
        assert_eq!(i["model"], MODEL);
        assert_eq!(i["input"][0]["type"], "image_url");
        assert_eq!(
            i["input"][0]["image_url"]["url"],
            "data:image/png;base64,AQID"
        );
        let t: Value = serde_json::from_str(&text_body("สวัสดี")).unwrap();
        assert_eq!(
            (t["model"].as_str(), t["input"].as_str()),
            (Some(MODEL), Some("สวัสดี"))
        );
    }

    // shape copied from the live response (2026-10-10), trimmed
    const CLEAN: &str = r#"{"id":"modr-1","model":"omni-moderation-latest","results":[{"flagged":false,
      "categories":{"harassment":false,"sexual":false,"violence":false,"illicit":null},
      "category_scores":{"harassment":0.0001,"sexual":0.002,"violence":0.0004,"illicit":0.00001},
      "category_applied_input_types":{"harassment":["text"],"sexual":["text"],"violence":["text"],"illicit":["text"]}}]}"#;

    #[test]
    fn parses_clean_and_flagged() {
        let v = parse(CLEAN).unwrap();
        assert!(!v.flagged && v.categories.is_empty());
        assert_eq!(v.scores[0].0, "sexual");
        let flagged = CLEAN
            .replace(r#""flagged":false"#, r#""flagged":true"#)
            .replace(r#""violence":false"#, r#""violence":true"#);
        let v = parse(&flagged).unwrap();
        assert!(v.flagged);
        assert_eq!(v.categories, vec!["violence".to_string()]);
    }

    #[test]
    fn malformed_is_an_error_never_a_pass() {
        assert!(parse("nope").is_err());
        assert!(parse(r#"{"results":[]}"#).is_err());
        assert!(parse(r#"{"results":[{"categories":{},"category_scores":{}}]}"#).is_err());
        // flagged without a category / category without flagged
        assert!(
            parse(
                r#"{"results":[{"flagged":true,"categories":{"x":false},"category_scores":{}}]}"#
            )
            .is_err()
        );
        assert!(parse(&CLEAN.replace(r#""violence":false"#, r#""violence":true"#)).is_err());
        // two results (array input of two) is not what we sent
        let two = format!(
            r#"{{"results":[{0},{0}]}}"#,
            &CLEAN[CLEAN.find("[{").unwrap() + 1..CLEAN.rfind("}]").unwrap() + 1]
        );
        assert!(parse(&two).is_err());
    }
}
