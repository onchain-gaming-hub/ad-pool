//! Game-owner config instructions (design §8). Signer must be `GameEntry.owner`
//! (platform-core, rev 3 #4). Only `init_ad_config` requires the game to be Active —
//! the others must keep working on a Suspended game (pc 03 §1 rule 3).

use anchor_lang::prelude::*;

use crate::{
    constants::*,
    error::AdPoolError,
    events::{AdConfigInitialized, AdConfigUpdated, SlotActiveSet, SlotAdded},
    math::min_bid_floor,
    platform_core::{self, accounts::GameEntry, types::GameStatus},
    state::{AdSlotSpec, GameAdConfig},
};

/// Instruction input for one slot (tuples are not supported by the Anchor IDL).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct AdSlotSpecInput {
    pub width: u16,
    pub height: u16,
}

/// Every rule a config must satisfy (design §2 · §4.2 · §5.3). Called on init and on
/// every update, so a config can never drift into an invalid state.
pub fn validate_config(
    genesis_ts: i64,
    epoch_duration_secs: i64,
    auction_close_lead_secs: i64,
    min_bid: u64,
    min_increment_bps: u16,
    bid_horizon_epochs: u16,
    rent: &Rent,
) -> Result<()> {
    require!(
        epoch_duration_secs > 0
            && epoch_duration_secs % SECS_PER_DAY == 0
            && epoch_duration_secs <= MAX_EPOCH_DURATION_SECS,
        AdPoolError::InvalidEpochDuration
    );
    // `-86_400 % 86_400 == 0` in Rust, so the sign check is explicit (Q7: UTC day start).
    require!(
        genesis_ts >= 0 && genesis_ts % SECS_PER_DAY == 0,
        AdPoolError::InvalidGenesis
    );
    require!(
        auction_close_lead_secs >= MIN_CLOSE_LEAD_SECS,
        AdPoolError::CloseLeadTooShort
    );
    require!(
        (1..=BPS_DENOMINATOR).contains(&min_increment_bps),
        AdPoolError::InvalidIncrement
    );
    let horizon_secs = (bid_horizon_epochs as i64)
        .checked_mul(epoch_duration_secs)
        .ok_or_else(|| error!(AdPoolError::MathOverflow))?;
    require!(
        horizon_secs <= MAX_HORIZON_SECS,
        AdPoolError::HorizonTooLong
    );
    // At least one epoch must be both inside the horizon and still open (design §4.2).
    let needed = auction_close_lead_secs
        .checked_add(epoch_duration_secs)
        .ok_or_else(|| error!(AdPoolError::MathOverflow))?;
    require!(horizon_secs >= needed, AdPoolError::HorizonTooShort);
    require!(
        min_bid >= min_bid_floor(rent)?,
        AdPoolError::MinBidBelowFloor
    );
    Ok(())
}

fn validate_slot(width: u16, height: u16) -> Result<AdSlotSpec> {
    require!(width > 0 && height > 0, AdPoolError::InvalidSlot);
    Ok(AdSlotSpec {
        width,
        height,
        active: true,
    })
}

// ------------------------------------------------------------------ init
// Note: Anchor runs the `init` CPI of `ad_config` before the constraints of the other
// fields (anchor-syn try_accounts.rs ~L300), so a stranger / Suspended game fails AFTER
// the create — the tx is atomic, nothing persists.

#[derive(Accounts)]
#[instruction(game_program: Pubkey)]
pub struct InitAdConfig<'info> {
    /// `GameEntry.owner` — pays the rent of the config (D11).
    #[account(mut, address = game_entry.owner @ AdPoolError::NotGameOwner)]
    pub game_owner: Signer<'info>,

    #[account(
        seeds = [PC_GAME_SEED, game_program.as_ref()],
        bump = game_entry.bump,
        seeds::program = platform_core::ID,
        constraint = matches!(game_entry.status, GameStatus::Active) @ AdPoolError::GameNotActive,
    )]
    pub game_entry: Account<'info, GameEntry>,

    #[account(
        init,
        payer = game_owner,
        space = 8 + GameAdConfig::INIT_SPACE,
        seeds = [AD_CONFIG_SEED, game_program.as_ref()],
        bump,
    )]
    pub ad_config: Account<'info, GameAdConfig>,

    pub system_program: Program<'info, System>,
}

#[allow(clippy::too_many_arguments)]
pub fn handle_init_ad_config(
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
    validate_config(
        genesis_ts,
        epoch_duration_secs,
        auction_close_lead_secs,
        min_bid,
        min_increment_bps,
        bid_horizon_epochs,
        &Rent::get()?,
    )?;
    // maintainer 2026-10-10 (design §4.4): genesis is immutable and there is no re-init, so a
    // far-future typo would brick the game's config forever → cap it at now + 90 days.
    let now = Clock::get()?.unix_timestamp;
    let max_genesis = now
        .checked_add(MAX_HORIZON_SECS)
        .ok_or_else(|| error!(AdPoolError::MathOverflow))?;
    require!(genesis_ts <= max_genesis, AdPoolError::InvalidGenesis);
    require!(!slots.is_empty(), AdPoolError::InvalidSlot);
    require!(slots.len() <= MAX_SLOTS as usize, AdPoolError::TooManySlots);

    let mut specs = [AdSlotSpec::default(); MAX_SLOTS as usize];
    for (i, s) in slots.iter().enumerate() {
        specs[i] = validate_slot(s.width, s.height)?;
    }

    let cfg = &mut ctx.accounts.ad_config;
    cfg.version = ACCOUNT_VERSION;
    cfg.bump = ctx.bumps.ad_config;
    cfg.slot_count = slots.len() as u8;
    cfg.game_program = game_program;
    cfg.genesis_ts = genesis_ts;
    cfg.epoch_duration_secs = epoch_duration_secs;
    cfg.auction_close_lead_secs = auction_close_lead_secs;
    cfg.min_bid = min_bid;
    cfg.min_increment_bps = min_increment_bps;
    cfg.bid_horizon_epochs = bid_horizon_epochs;
    cfg.slots = specs;
    cfg.created_at = now;
    cfg.updated_at = now;
    cfg._reserved = [0; 64];

    emit!(AdConfigInitialized {
        game_program,
        game_owner: ctx.accounts.game_owner.key(),
        genesis_ts,
        epoch_duration_secs,
        auction_close_lead_secs,
        min_bid,
        min_increment_bps,
        bid_horizon_epochs,
        slot_count: cfg.slot_count,
    });
    // One SlotAdded per initial slot, so an indexer learns every W×H from events alone.
    for (i, s) in specs.iter().take(slots.len()).enumerate() {
        emit!(SlotAdded {
            game_program,
            slot_index: i as u8,
            width: s.width,
            height: s.height,
        });
    }
    Ok(())
}

// --------------------------------------------------------- owner-only edits

/// Shared accounts of the owner-only edits. No Active check (pc 03 §1 rule 3).
#[derive(Accounts)]
pub struct ManageAdConfig<'info> {
    #[account(address = game_entry.owner @ AdPoolError::NotGameOwner)]
    pub game_owner: Signer<'info>,

    #[account(
        seeds = [PC_GAME_SEED, ad_config.game_program.as_ref()],
        bump = game_entry.bump,
        seeds::program = platform_core::ID,
    )]
    pub game_entry: Account<'info, GameEntry>,

    #[account(
        mut,
        seeds = [AD_CONFIG_SEED, ad_config.game_program.as_ref()],
        bump = ad_config.bump,
    )]
    pub ad_config: Account<'info, GameAdConfig>,
}

/// `None` = keep (an all-`None` update is allowed: bumps `updated_at`, emits current values —
/// it still re-validates, so it fails if cluster rent ever rose above the stored `min_bid`).
/// `genesis_ts` and `epoch_duration_secs` are immutable (design §4.4).
/// New values apply to auctions opened afterwards only (existing ones keep snapshots).
pub fn handle_update_ad_config(
    ctx: Context<ManageAdConfig>,
    auction_close_lead_secs: Option<i64>,
    min_bid: Option<u64>,
    min_increment_bps: Option<u16>,
    bid_horizon_epochs: Option<u16>,
) -> Result<()> {
    let cfg = &mut ctx.accounts.ad_config;
    let lead = auction_close_lead_secs.unwrap_or(cfg.auction_close_lead_secs);
    let min_bid = min_bid.unwrap_or(cfg.min_bid);
    let inc = min_increment_bps.unwrap_or(cfg.min_increment_bps);
    let horizon = bid_horizon_epochs.unwrap_or(cfg.bid_horizon_epochs);
    validate_config(
        cfg.genesis_ts,
        cfg.epoch_duration_secs,
        lead,
        min_bid,
        inc,
        horizon,
        &Rent::get()?,
    )?;
    cfg.auction_close_lead_secs = lead;
    cfg.min_bid = min_bid;
    cfg.min_increment_bps = inc;
    cfg.bid_horizon_epochs = horizon;
    cfg.updated_at = Clock::get()?.unix_timestamp;
    emit!(AdConfigUpdated {
        game_program: cfg.game_program,
        auction_close_lead_secs: lead,
        min_bid,
        min_increment_bps: inc,
        bid_horizon_epochs: horizon,
    });
    Ok(())
}

/// Append a slot. Slots are never removed or resized (auctions reference the index).
pub fn handle_add_slot(ctx: Context<ManageAdConfig>, width: u16, height: u16) -> Result<()> {
    let cfg = &mut ctx.accounts.ad_config;
    require!(cfg.slot_count < MAX_SLOTS, AdPoolError::TooManySlots);
    let index = cfg.slot_count;
    cfg.slots[index as usize] = validate_slot(width, height)?;
    cfg.slot_count += 1;
    cfg.updated_at = Clock::get()?.unix_timestamp;
    emit!(SlotAdded {
        game_program: cfg.game_program,
        slot_index: index,
        width,
        height,
    });
    Ok(())
}

/// `false` stops new auctions on the slot; existing auctions continue.
pub fn handle_set_slot_active(
    ctx: Context<ManageAdConfig>,
    slot_index: u8,
    active: bool,
) -> Result<()> {
    let cfg = &mut ctx.accounts.ad_config;
    require!(slot_index < cfg.slot_count, AdPoolError::InvalidSlot);
    cfg.slots[slot_index as usize].active = active;
    cfg.updated_at = Clock::get()?.unix_timestamp;
    emit!(SlotActiveSet {
        game_program: cfg.game_program,
        slot_index,
        active,
    });
    Ok(())
}
