//! Wallet-signature auth (design §5 · CLAUDE.md gotcha 11). The winner signs a MESSAGE
//! (never a transaction) with their External Wallet (`signMessage`). The **Worker builds
//! the message itself** from its own config + the submitted fields + the payload hash it
//! computed, and verifies the signature over those exact bytes — the client sends only
//! `nonce` + `expires`. Nothing to parse, no canonical-form ambiguity, and multipart's
//! LF→CRLF rewriting of text fields cannot break it.
//!
//! The message (UTF-8, `\n` line endings, no trailing newline) — FE renders the same template:
//! ```text
//! ad-pool creative submission
//! domain: <AUTH_DOMAIN>
//! cluster: devnet
//! program: <ad-pool program id>
//! game: <game_program>
//! epoch: <u64>
//! slot: <u8>
//! payload: <sha256 hex of the payload, lowercase>
//! nonce: <32 lowercase hex chars>
//! expires: <unix seconds>
//! ```

use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;
use solana_signature::Signature;

pub const HEADER: &str = "ad-pool creative submission";
/// W4: a different first line, so a creative signature can never pass as a default-ad one
/// (and vice versa) even if every other line matched.
pub const DEFAULT_AD_HEADER: &str = "ad-pool default ad submission";
/// A signed message is valid for at most this long (and must not be expired).
pub const MAX_VALIDITY_SECS: i64 = 600;
pub const NONCE_HEX_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedMessage {
    pub domain: String,
    pub cluster: String,
    pub program: Pubkey,
    pub game: Pubkey,
    pub epoch: u64,
    pub slot: u8,
    pub payload: [u8; 32],
    pub nonce: String,
    pub expires: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum AuthError {
    BadNonce,
    BadSignatureEncoding,
    BadSignature,
    Expired,
    TooLongValid,
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl SignedMessage {
    pub fn render(&self) -> String {
        format!(
            "{HEADER}\ndomain: {}\ncluster: {}\nprogram: {}\ngame: {}\nepoch: {}\nslot: {}\npayload: {}\nnonce: {}\nexpires: {}",
            self.domain,
            self.cluster,
            self.program,
            self.game,
            self.epoch,
            self.slot,
            hex(&self.payload),
            self.nonce,
            self.expires
        )
    }
}

/// W4 `POST /v1/default-ad` — signed by a platform-core **Admin** wallet (pc Q-A):
/// ```text
/// ad-pool default ad submission
/// domain: <AUTH_DOMAIN>
/// cluster: devnet
/// program: <platform-core program id>
/// width: <u16>
/// height: <u16>
/// payload: <sha256 hex — same payload_hash as a full creative>
/// nonce: <32 lowercase hex chars>
/// expires: <unix seconds>
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultAdMessage {
    pub domain: String,
    pub cluster: String,
    pub program: Pubkey,
    pub width: u16,
    pub height: u16,
    pub payload: [u8; 32],
    pub nonce: String,
    pub expires: i64,
}

impl DefaultAdMessage {
    pub fn render(&self) -> String {
        format!(
            "{DEFAULT_AD_HEADER}\ndomain: {}\ncluster: {}\nprogram: {}\nwidth: {}\nheight: {}\npayload: {}\nnonce: {}\nexpires: {}",
            self.domain,
            self.cluster,
            self.program,
            self.width,
            self.height,
            hex(&self.payload),
            self.nonce,
            self.expires
        )
    }
}

/// sha256 of the creative payload the signature commits to:
/// `"ad-pool/v1" ‖ image_sha256 ‖ u32le(len text) ‖ text ‖ u32le(len click_url) ‖ click_url`.
/// Image-only resubmissions (status TextOnly) use empty text/click_url.
pub fn payload_hash(image_sha256: &[u8; 32], text: &str, click_url: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"ad-pool/v1");
    h.update(image_sha256);
    h.update((text.len() as u32).to_le_bytes());
    h.update(text.as_bytes());
    h.update((click_url.len() as u32).to_le_bytes());
    h.update(click_url.as_bytes());
    h.finalize().into()
}

pub fn sha256(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}

pub fn valid_nonce(n: &str) -> bool {
    n.len() == NONCE_HEX_LEN
        && n.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Check nonce shape + expiry window, then the ed25519 signature of `signer` over
/// `msg.render()` (the message the Worker built).
pub fn verify(
    msg: &SignedMessage,
    signature_b58: &str,
    signer: &Pubkey,
    now: i64,
) -> Result<(), AuthError> {
    verify_rendered(
        &msg.render(),
        &msg.nonce,
        msg.expires,
        signature_b58,
        signer,
        now,
    )
}

/// Same checks for any Worker-built message text (`SignedMessage` / `DefaultAdMessage`).
pub fn verify_rendered(
    rendered: &str,
    nonce: &str,
    expires: i64,
    signature_b58: &str,
    signer: &Pubkey,
    now: i64,
) -> Result<(), AuthError> {
    if !valid_nonce(nonce) {
        return Err(AuthError::BadNonce);
    }
    if expires <= now {
        return Err(AuthError::Expired);
    }
    if expires > now + MAX_VALIDITY_SECS {
        return Err(AuthError::TooLongValid);
    }
    let sig: Signature = signature_b58
        .parse()
        .map_err(|_| AuthError::BadSignatureEncoding)?;
    if !sig.verify(signer.as_ref(), rendered.as_bytes()) {
        return Err(AuthError::BadSignature);
    }
    Ok(())
}
