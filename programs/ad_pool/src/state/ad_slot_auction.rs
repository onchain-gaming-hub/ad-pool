use anchor_lang::prelude::*;

use super::{AdCreative, CreativeStatus};

/// PDA `[AD_SLOT_SEED, game_program, epoch.to_le_bytes(), &[slot_index]]` — one per
/// (game, epoch, slot); state + escrow of the highest bid (design §3). 779 bytes.
/// Created by `open_auction` (the first bid), so there is always a leader.
#[account]
#[derive(InitSpace, Debug)]
pub struct AdSlotAuction {
    /// offset 8
    pub version: u8,
    /// offset 9
    pub bump: u8,
    /// offset 10 — memcmp-able.
    pub creative_status: CreativeStatus,
    /// offset 11 — memcmp-able (crank finds unsettled auctions).
    pub settled: bool,
    /// offset 12 — duplicates the seed.
    pub slot_index: u8,
    /// offset 13 — duplicates the seed.
    pub game_program: Pubkey,
    /// offset 45 — duplicates the seed.
    pub epoch: u64,
    /// offset 53 — snapshot of slots[slot_index].
    pub width: u16,
    /// offset 55
    pub height: u16,
    /// offset 57 — epoch_start_ts − auction_close_lead_secs (snapshot).
    pub close_ts: i64,
    /// offset 65 — genesis_ts + epoch × epoch_duration_secs (snapshot).
    pub epoch_start_ts: i64,
    /// offset 73 — epoch_start_ts + epoch_duration_secs (snapshot).
    pub epoch_end_ts: i64,
    /// offset 81 — PlatformConfig.ad_fee_bps at open_auction (rev 3 #7).
    pub fee_bps: u16,
    /// offset 83 — snapshot.
    pub min_increment_bps: u16,
    /// offset 85 — the first bidder; gets the rent back on close (D11). Fixed offset.
    pub rent_payer: Pubkey,
    /// offset 117 — never empty (account is born with a bid).
    pub highest_bidder: Pubkey,
    /// offset 149 — winning amount; held in escrow until settle, history after.
    pub highest_bid: u64,
    /// offset 157
    pub bid_count: u32,
    /// offset 161
    pub reject_count: u16,
    /// offset 163 — bitmask of the latest reject/revoke reason (design §7.3).
    pub last_reject_flags: u32,
    /// offset 167 — last approve time; 0 = none (reset by revoke).
    pub approved_at: i64,
    /// offset 175
    pub updated_at: i64,
    /// offset 183 — before `creative` so every fixed field keeps a fixed offset.
    pub _reserved: [u8; 32],
    /// offset 215 — variable length (max 564).
    pub creative: AdCreative,
}
