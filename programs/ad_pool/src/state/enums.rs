use anchor_lang::prelude::*;

/// What the FE may show for an auction (design §4.1). Time-based phases are NOT stored —
/// they are computed from `Clock` (`math::phase`).
/// ⚠️ Never reorder. INVARIANT: every explicit value == its variant index (the IDL carries
/// no discriminant values; `declare_program!` consumers encode the index). Append only.
/// Guarded by tests/test_layout.rs.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum CreativeStatus {
    /// Nothing approved → FE shows platform-core's DefaultAd.
    Empty = 0,
    /// text + click_url approved, image not (A13) → FE shows the text.
    TextOnly = 1,
    /// Full creative approved → FE shows the image (verify sha256 first).
    Approved = 2,
}
