use anchor_lang::prelude::*;

use crate::constants::{AD_CLICK_URL_MAX_LEN, AD_IMAGE_URL_MAX_LEN, AD_TEXT_MAX_LEN};

/// Image + text + click URL (A10).
/// ⚠️ Must match `platform_core::AdCreative` field-for-field, including every `max_len`
/// (bytes, not characters) — the FE renders both with one renderer (pc 01 §7).
/// `#[max_len]` only sizes the account; lengths are enforced with `require!` in the ix.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq, InitSpace, Default)]
pub struct AdCreative {
    /// R2 object URL (`ads/<sha256>.<ext>`). Empty while `TextOnly`.
    #[max_len(AD_IMAGE_URL_MAX_LEN)]
    pub image_url: String,
    /// FE must check the fetched image hashes to this. All zeros while `TextOnly`.
    pub image_sha256: [u8; 32],
    /// Fallback shown instead of the image (A13).
    #[max_len(AD_TEXT_MAX_LEN)]
    pub text: String,
    #[max_len(AD_CLICK_URL_MAX_LEN)]
    pub click_url: String,
}
