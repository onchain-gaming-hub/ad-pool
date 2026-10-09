//! Events (`emit!` → program log, same as platform-core). Events that create or close
//! an account carry `rent_payer` (design §12).

use anchor_lang::prelude::*;

use crate::state::CreativeStatus;

#[event]
pub struct AdConfigInitialized {
    pub game_program: Pubkey,
    pub game_owner: Pubkey,
    pub genesis_ts: i64,
    pub epoch_duration_secs: i64,
    pub auction_close_lead_secs: i64,
    pub min_bid: u64,
    pub min_increment_bps: u16,
    pub bid_horizon_epochs: u16,
    pub slot_count: u8,
}

#[event]
pub struct AdConfigUpdated {
    pub game_program: Pubkey,
    pub auction_close_lead_secs: i64,
    pub min_bid: u64,
    pub min_increment_bps: u16,
    pub bid_horizon_epochs: u16,
}

#[event]
pub struct SlotAdded {
    pub game_program: Pubkey,
    pub slot_index: u8,
    pub width: u16,
    pub height: u16,
}

#[event]
pub struct SlotActiveSet {
    pub game_program: Pubkey,
    pub slot_index: u8,
    pub active: bool,
}

#[event]
pub struct AuctionOpened {
    pub game_program: Pubkey,
    pub epoch: u64,
    pub slot_index: u8,
    pub rent_payer: Pubkey,
    pub fee_bps: u16,
    pub close_ts: i64,
    pub epoch_start_ts: i64,
    pub epoch_end_ts: i64,
}

#[event]
pub struct BidPlaced {
    pub game_program: Pubkey,
    pub epoch: u64,
    pub slot_index: u8,
    pub bidder: Pubkey,
    /// New total bid.
    pub amount: u64,
    /// Previous leader refunded in the same tx (None on first bid / self-raise).
    pub refunded_to: Option<Pubkey>,
    pub refunded: u64,
}

#[event]
pub struct CreativeApproved {
    /// Signer holding platform-core's Moderator role.
    pub moderator: Pubkey,
    pub game_program: Pubkey,
    pub epoch: u64,
    pub slot_index: u8,
    pub status: CreativeStatus,
    pub image_sha256: [u8; 32],
    /// Non-zero only for a text-only approval whose image was rejected.
    pub image_reject_flags: u32,
}

#[event]
pub struct CreativeRejected {
    pub moderator: Pubkey,
    pub game_program: Pubkey,
    pub epoch: u64,
    pub slot_index: u8,
    pub flags: u32,
    pub reject_count: u16,
}

#[event]
pub struct CreativeRevoked {
    pub moderator: Pubkey,
    pub game_program: Pubkey,
    pub epoch: u64,
    pub slot_index: u8,
    pub prev_status: CreativeStatus,
    pub flags: u32,
}

#[event]
pub struct Settled {
    pub game_program: Pubkey,
    pub epoch: u64,
    pub slot_index: u8,
    pub fee: u64,
    pub net: u64,
    pub game_owner: Pubkey,
}

#[event]
pub struct AuctionClosed {
    pub game_program: Pubkey,
    pub epoch: u64,
    pub slot_index: u8,
    pub rent_payer: Pubkey,
    /// Lamports returned to rent_payer.
    pub refunded: u64,
}
