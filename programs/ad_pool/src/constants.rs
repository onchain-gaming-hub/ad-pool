//! Seeds and limits. ⚠️ Interface — never change a seed value after deploy.
//! Design: specs/designs/01-program-design-v1.md §2–§5.

use anchor_lang::prelude::*;

/// `[AD_CONFIG_SEED, game_program]`
#[constant]
pub const AD_CONFIG_SEED: &[u8] = b"ad_config";

/// `[AD_SLOT_SEED, game_program, epoch.to_le_bytes(), &[slot_index]]`
#[constant]
pub const AD_SLOT_SEED: &[u8] = b"ad_slot";

/// Layout version written into byte 8 of every account at `init`.
#[constant]
pub const ACCOUNT_VERSION: u8 = 1;

/// Fixed length of `GameAdConfig.slots`.
#[constant]
pub const MAX_SLOTS: u8 = 8;

#[constant]
pub const SECS_PER_DAY: i64 = 86_400;

/// D5: bidding closes at least 7 days before an epoch starts.
#[constant]
pub const MIN_CLOSE_LEAD_SECS: i64 = 7 * SECS_PER_DAY;

/// Q11: cap on one epoch — bounds how long an auction's rent can stay locked.
#[constant]
pub const MAX_EPOCH_DURATION_SECS: i64 = 30 * SECS_PER_DAY;

/// Q11: cap on `bid_horizon_epochs × epoch_duration_secs`.
#[constant]
pub const MAX_HORIZON_SECS: i64 = 90 * SECS_PER_DAY;

/// Fee ceiling (C2). ad-pool re-checks the snapshot it takes from platform-core
/// and fails closed above it (design §5.3).
#[constant]
pub const MAX_FEE_BPS: u16 = 2_000;

#[constant]
pub const BPS_DENOMINATOR: u16 = 10_000;

/// `AdCreative` limits in BYTES (Thai ≈ 3 bytes per character).
/// ⚠️ Must equal platform-core's (pc 01 §7) — value AND type (u16) — shared FE renderer.
#[constant]
pub const AD_IMAGE_URL_MAX_LEN: u16 = 200;
#[constant]
pub const AD_TEXT_MAX_LEN: u16 = 120;
#[constant]
pub const AD_CLICK_URL_MAX_LEN: u16 = 200;

// platform-core seeds (pc 01 §9). Accounts of platform-core are always checked with
// these seeds + `seeds::program = platform_core::ID` (pc 03 §1).
pub const PC_CONFIG_SEED: &[u8] = b"config";
pub const PC_TREASURY_SEED: &[u8] = b"treasury";
pub const PC_ROLE_SEED: &[u8] = b"role";
pub const PC_GAME_SEED: &[u8] = b"game";

// ---- account sizes + memcmp offsets (Worker crank / FE `getProgramAccounts` filters)

/// `GameAdConfig` data size (discriminator included). u32 like the offsets (TS `dataSize`).
/// Tied to `INIT_SPACE` by a compile-time assert in math.rs.
#[constant]
pub const GAME_AD_CONFIG_SIZE: u32 = 199;
/// `AdSlotAuction` data size (discriminator included, creative fully reserved).
#[constant]
pub const AD_SLOT_AUCTION_SIZE: u32 = 779;
/// `AdSlotAuction.creative_status` (u8: 0 Empty · 1 TextOnly · 2 Approved).
#[constant]
pub const CREATIVE_STATUS_OFFSET: u32 = 10;
/// `AdSlotAuction.settled` (bool) — crank finds unsettled auctions with byte 0x00.
#[constant]
pub const SETTLED_OFFSET: u32 = 11;
/// `AdSlotAuction.rent_payer` (Pubkey).
#[constant]
pub const RENT_PAYER_OFFSET: u32 = 85;
/// `AdSlotAuction.creative` (start of the variable-length tail).
#[constant]
pub const CREATIVE_OFFSET: u32 = 215;

// ---- reject / revoke reason bitmask (design §7.3) — Worker writes, FE/indexer reads.
#[constant]
pub const REJECT_FILE_TYPE: u32 = 1 << 0;
#[constant]
pub const REJECT_DIMENSIONS: u32 = 1 << 1;
#[constant]
pub const REJECT_IMAGE_MODERATION: u32 = 1 << 2;
#[constant]
pub const REJECT_TEXT_MODERATION: u32 = 1 << 3;
#[constant]
pub const REJECT_URL_RULES: u32 = 1 << 4;
#[constant]
pub const REJECT_URL_REPUTATION: u32 = 1 << 5;
#[constant]
pub const REJECT_TOO_LONG: u32 = 1 << 6;

// The cap must leave a positive net (min_bid_floor's payout term divides by the difference).
const _: () = assert!(MAX_FEE_BPS < BPS_DENOMINATOR);
