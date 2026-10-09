use anchor_lang::prelude::*;

use crate::constants::MAX_SLOTS;

/// One ad slot of a game (5 bytes).
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace,
)]
pub struct AdSlotSpec {
    pub width: u16,
    pub height: u16,
    /// false = no new auction for this slot (existing ones continue).
    pub active: bool,
}

/// PDA `[AD_CONFIG_SEED, game_program]` — one per game (design §2). 199 bytes.
/// Created by `GameEntry.owner`; there is no owner/revenue field here (rev 3 #4).
#[account]
#[derive(InitSpace, Debug)]
pub struct GameAdConfig {
    /// offset 8
    pub version: u8,
    /// offset 9
    pub bump: u8,
    /// offset 10 — slots in use (≤ MAX_SLOTS).
    pub slot_count: u8,
    /// offset 11 — duplicates the seed.
    pub game_program: Pubkey,
    /// offset 43 — start of epoch 0; immutable; multiple of 86,400 (Q7).
    pub genesis_ts: i64,
    /// offset 51 — multiple of 86,400, ≤ MAX_EPOCH_DURATION_SECS; immutable (design §4.4).
    pub epoch_duration_secs: i64,
    /// offset 59 — ≥ MIN_CLOSE_LEAD_SECS (D5); snapshot into each new auction.
    pub auction_close_lead_secs: i64,
    /// offset 67 — lamports (A12); ≥ min_bid_floor.
    pub min_bid: u64,
    /// offset 75 — 1..=10,000; snapshot into each new auction.
    pub min_increment_bps: u16,
    /// offset 77 — ≥ 1; × duration ≤ MAX_HORIZON_SECS.
    pub bid_horizon_epochs: u16,
    /// offset 79 — fixed array (not Vec) so every offset stays constant. Append-only.
    pub slots: [AdSlotSpec; MAX_SLOTS as usize],
    /// offset 119
    pub created_at: i64,
    /// offset 127
    pub updated_at: i64,
    /// offset 135
    pub _reserved: [u8; 64],
}
