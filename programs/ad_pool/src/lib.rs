//! ad-pool — per-game ad slot auctions in SOL, one auction per (game, epoch, slot),
//! with creatives written on chain by a Moderator-role Worker after off-chain checks.
//! Design: specs/designs/01-program-design-v1.md · Plan: specs/designs/02-implementation-plan.md
//!
//! Phases: P0 layouts · P1 config instructions · P2 open_auction + bid.

pub mod constants;
pub mod error;
pub mod events;
pub mod instructions;
pub mod math;
pub mod state;

use anchor_lang::prelude::*;

pub use constants::*;
pub use error::*;
pub use events::*;
pub use instructions::*;
pub use state::*;

declare_id!("6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk");

// platform-core is read through its IDL only (idls/platform_core.json) — no crate
// dependency (pc 03 §0). Types generated here have no `PartialEq`: use `matches!`.
declare_program!(platform_core);

#[program]
pub mod ad_pool {
    use super::*;

    /// GameEntry.owner of an Active game. One config per game.
    #[allow(clippy::too_many_arguments)]
    pub fn init_ad_config(
        ctx: Context<InitAdConfig>,
        game_program: Pubkey,
        genesis_ts: i64,
        epoch_duration_secs: i64,
        auction_close_lead_secs: i64,
        min_bid: u64,
        min_increment_bps: u16,
        bid_horizon_epochs: u16,
        slots: Vec<AdSlotSpecInput>,
    ) -> Result<()> {
        instructions::config::handle_init_ad_config(
            ctx,
            game_program,
            genesis_ts,
            epoch_duration_secs,
            auction_close_lead_secs,
            min_bid,
            min_increment_bps,
            bid_horizon_epochs,
            slots,
        )
    }

    /// GameEntry.owner. `None` keeps the current value; genesis/duration are immutable.
    pub fn update_ad_config(
        ctx: Context<ManageAdConfig>,
        auction_close_lead_secs: Option<i64>,
        min_bid: Option<u64>,
        min_increment_bps: Option<u16>,
        bid_horizon_epochs: Option<u16>,
    ) -> Result<()> {
        instructions::config::handle_update_ad_config(
            ctx,
            auction_close_lead_secs,
            min_bid,
            min_increment_bps,
            bid_horizon_epochs,
        )
    }

    /// GameEntry.owner. Append-only.
    pub fn add_slot(ctx: Context<ManageAdConfig>, width: u16, height: u16) -> Result<()> {
        instructions::config::handle_add_slot(ctx, width, height)
    }

    /// GameEntry.owner. `false` = no new auctions on this slot.
    pub fn set_slot_active(
        ctx: Context<ManageAdConfig>,
        slot_index: u8,
        active: bool,
    ) -> Result<()> {
        instructions::config::handle_set_slot_active(ctx, slot_index, active)
    }

    /// First bid of (game, epoch, slot): creates the escrow, bidder pays its rent.
    pub fn open_auction(
        ctx: Context<OpenAuction>,
        game_program: Pubkey,
        epoch: u64,
        slot_index: u8,
        amount: u64,
    ) -> Result<()> {
        instructions::auction::handle_open_auction(ctx, game_program, epoch, slot_index, amount)
    }

    /// Outbid (refunds the previous leader in the same tx) or raise your own bid.
    /// `amount` is the new total.
    pub fn bid(ctx: Context<Bid>, amount: u64) -> Result<()> {
        instructions::auction::handle_bid(ctx, amount)
    }
}
