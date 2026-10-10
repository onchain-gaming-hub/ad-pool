//! Approved images in R2 (A4) and how they are served (`GET /ads/<sha256>.<ext>` — maintainer
//! 2026-10-10: images go through the Worker). Key = content hash, so an object can never be
//! replaced by different bytes, and FE checks the bytes against `image_sha256` on chain.
//! Objects are written **only after the image passed moderation** (A1).

use crate::{auth, image::ImageKind};

/// `ads/<64 hex>.<ext>`
pub fn object_key(sha: &[u8; 32], kind: ImageKind) -> String {
    format!("ads/{}.{}", auth::hex(sha), kind.ext())
}

/// On-chain `image_url` (≤ 200 bytes — guaranteed by `config::MAX_PUBLIC_BASE_URL_LEN`).
pub fn image_url(public_base_url: &str, sha: &[u8; 32], kind: ImageKind) -> String {
    format!("{public_base_url}/{}", object_key(sha, kind))
}

/// Parses the `<file>` of `GET /ads/<file>` — only exact `<64 lowercase hex>.<png|jpg|webp>`.
pub fn parse_file(file: &str) -> Option<(String, ImageKind)> {
    let (hex, ext) = file.split_once('.')?;
    let kind = match ext {
        "png" => ImageKind::Png,
        "jpg" => ImageKind::Jpeg,
        "webp" => ImageKind::Webp,
        _ => return None,
    };
    let ok = hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    ok.then(|| (format!("ads/{file}"), kind))
}

/// Response headers for a served image. The content type comes from the key's extension,
/// which the Worker derived from the magic bytes it checked — never from the uploader.
pub fn serve_headers(kind: ImageKind, sha_hex: &str) -> [(&'static str, String); 6] {
    [
        ("content-type", kind.mime().to_string()),
        ("x-content-type-options", "nosniff".to_string()),
        // content-addressed → never changes
        (
            "cache-control",
            "public, max-age=31536000, immutable".to_string(),
        ),
        ("etag", format!("\"{sha_hex}\"")),
        // an image response must not become a document that runs anything
        (
            "content-security-policy",
            "default-src 'none'; sandbox".to_string(),
        ),
        ("cross-origin-resource-policy", "cross-origin".to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_urls_and_parsing_round_trip() {
        let sha = [0xab; 32];
        let k = object_key(&sha, ImageKind::Webp);
        assert_eq!(k, format!("ads/{}.webp", "ab".repeat(32)));
        let u = image_url("https://w.example", &sha, ImageKind::Jpeg);
        assert_eq!(u, format!("https://w.example/ads/{}.jpg", "ab".repeat(32)));
        let file = k.strip_prefix("ads/").unwrap();
        assert_eq!(parse_file(file), Some((k.clone(), ImageKind::Webp)));
        // worst case fits the on-chain limit
        let base = format!(
            "https://{}",
            "a".repeat(crate::config::MAX_PUBLIC_BASE_URL_LEN - 8)
        );
        assert_eq!(image_url(&base, &sha, ImageKind::Webp).len(), 200);
    }

    #[test]
    fn rejects_other_files() {
        let h = "ab".repeat(32);
        for f in [
            format!("{h}.gif"),
            format!("{h}.PNG"),
            format!("{}.png", "AB".repeat(32)),
            format!("{}.png", &h[1..]),
            format!("{h}.png.png"),
            format!("../{h}.png"),
            h.clone(),
            "x.png".to_string(),
        ] {
            assert_eq!(parse_file(&f), None, "{f}");
        }
    }

    #[test]
    fn serve_headers_are_strict() {
        let h = serve_headers(ImageKind::Png, "00");
        assert!(h.contains(&("content-type", "image/png".into())));
        assert!(h.contains(&("x-content-type-options", "nosniff".into())));
        assert!(
            h.iter()
                .any(|(k, v)| *k == "cache-control" && v.contains("immutable"))
        );
    }
}
