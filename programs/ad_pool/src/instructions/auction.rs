//! Bidding (design §5). `open_auction` = first bid of a (game, epoch, slot): creates the
//! escrow account, paid by the bidder (Q6: External Wallet only → `payer = bidder`).
//! `bid` = every later bid; refunds the previous leader in the same tx (A3).
//! Every bid requires the game to be Active (Q2 · pc spec rev 3 #7 "no new bids").

use anchor_lang::{
    prelude::*,
    system_program::{transfer, Transfer},
};

use crate::{
    constants::*,
    error::AdPoolError,
    events::{AuctionOpened, BidPlaced},
    math::{auction_window, epoch_start_ts, min_bid_floor, min_next_bid, AD_SLOT_AUCTION_SPACE},
    platform_core::{
        self,
        accounts::{GameEntry, PlatformConfig},
        types::GameStatus,
    },
    state::{AdCreative, AdSlotAuction, CreativeStatus, GameAdConfig},
};

fn pay_into_escrow<'info>(
    system_program: &Program<'info, System>,
    from: &Signer<'info>,
    to: &AccountInfo<'info>,
    lamports: u64,
) -> Result<()> {
    transfer(
        CpiContext::new(
            system_program.key(),
            Transfer {
                from: from.to_account_info(),
                to: to.clone(),
            },
        ),
        lamports,
    )
}

// ------------------------------------------------------------- open_auction
// Note: Anchor runs the `init` CPI of `auction` before the other fields' constraints
// (anchor-syn try_accounts.rs ~L300); a failing check later reverts it atomically.

#[derive(Accounts)]
#[instruction(game_program: Pubkey, epoch: u64, slot_index: u8)]
pub struct OpenAuction<'info> {
    /// Bids and pays the auction's rent (`rent_payer`).
    #[account(mut)]
    pub bidder: Signer<'info>,

    #[account(
        seeds = [PC_GAME_SEED, game_program.as_ref()],
        bump = game_entry.bump,
        seeds::program = platform_core::ID,
        constraint = matches!(game_entry.status, GameStatus::Active) @ AdPoolError::GameNotActive,
    )]
    pub game_entry: Account<'info, GameEntry>,

    /// Source of the fee snapshot (rev 3 #7 · pc 03 §2.5).
    #[account(
        seeds = [PC_CONFIG_SEED],
        bump = config.bump,
        seeds::program = platform_core::ID,
    )]
    pub config: Account<'info, PlatformConfig>,

    #[account(
        seeds = [AD_CONFIG_SEED, game_program.as_ref()],
        bump = ad_config.bump,
    )]
    pub ad_config: Account<'info, GameAdConfig>,

    #[account(
        init,
        payer = bidder,
        space = AD_SLOT_AUCTION_SPACE,
        seeds = [AD_SLOT_SEED, game_program.as_ref(), &epoch.to_le_bytes(), &[slot_index]],
        bump,
    )]
    pub auction: Account<'info, AdSlotAuction>,

    pub system_program: Program<'info, System>,
}

pub fn handle_open_auction(
    ctx: Context<OpenAuction>,
    game_program: Pubkey,
    epoch: u64,
    slot_index: u8,
    amount: u64,
) -> Result<()> {
    let cfg = &ctx.accounts.ad_config;
    // index first: unused slots are zeroed with active = false (InvalidSlot, not SlotInactive)
    require!(slot_index < cfg.slot_count, AdPoolError::InvalidSlot);
    let slot = cfg.slots[slot_index as usize];
    require!(slot.active, AdPoolError::SlotInactive);

    let fee_bps = ctx.accounts.config.ad_fee_bps;
    require!(fee_bps <= MAX_FEE_BPS, AdPoolError::FeeTooHigh);

    // time rules (design §4.2)
    let now = Clock::get()?.unix_timestamp;
    let start = epoch_start_ts(cfg.genesis_ts, cfg.epoch_duration_secs, epoch)?;
    let (close_ts, epoch_end_ts) =
        auction_window(start, cfg.epoch_duration_secs, cfg.auction_close_lead_secs)?;
    require!(now < close_ts, AdPoolError::BiddingClosed);
    let horizon_end = (cfg.bid_horizon_epochs as i64)
        .checked_mul(cfg.epoch_duration_secs)
        .and_then(|h| now.checked_add(h))
        .ok_or_else(|| error!(AdPoolError::MathOverflow))?;
    require!(start <= horizon_end, AdPoolError::BeyondHorizon);

    // amount (design §5.2 · §5.3)
    require!(
        amount >= min_bid_floor(&Rent::get()?)?,
        AdPoolError::BidBelowFloor
    );
    require!(amount >= cfg.min_bid, AdPoolError::BidTooLow);

    let bidder = ctx.accounts.bidder.key();
    let a = &mut ctx.accounts.auction;
    a.version = ACCOUNT_VERSION;
    a.bump = ctx.bumps.auction;
    a.creative_status = CreativeStatus::Empty;
    a.settled = false;
    a.slot_index = slot_index;
    a.game_program = game_program;
    a.epoch = epoch;
    a.width = slot.width;
    a.height = slot.height;
    a.close_ts = close_ts;
    a.epoch_start_ts = start;
    a.epoch_end_ts = epoch_end_ts;
    a.fee_bps = fee_bps;
    a.min_increment_bps = cfg.min_increment_bps;
    a.rent_payer = bidder;
    a.highest_bidder = bidder;
    a.highest_bid = amount;
    a.bid_count = 1;
    a.reject_count = 0;
    a.last_reject_flags = 0;
    a.approved_at = 0;
    a.updated_at = now;
    a._reserved = [0; 32];
    a.creative = AdCreative::default();

    pay_into_escrow(
        &ctx.accounts.system_program,
        &ctx.accounts.bidder,
        &ctx.accounts.auction.to_account_info(),
        amount,
    )?;

    emit!(AuctionOpened {
        game_program,
        epoch,
        slot_index,
        rent_payer: bidder,
        fee_bps,
        close_ts,
        epoch_start_ts: start,
        epoch_end_ts,
    });
    emit!(BidPlaced {
        game_program,
        epoch,
        slot_index,
        bidder,
        amount,
        refunded_to: None,
        refunded: 0,
    });
    Ok(())
}

// ---------------------------------------------------------------------- bid

#[derive(Accounts)]
pub struct Bid<'info> {
    #[account(mut)]
    pub bidder: Signer<'info>,

    #[account(
        mut,
        seeds = [
            AD_SLOT_SEED,
            auction.game_program.as_ref(),
            &auction.epoch.to_le_bytes(),
            &[auction.slot_index],
        ],
        bump = auction.bump,
    )]
    pub auction: Account<'info, AdSlotAuction>,

    #[account(
        seeds = [PC_GAME_SEED, auction.game_program.as_ref()],
        bump = game_entry.bump,
        seeds::program = platform_core::ID,
        constraint = matches!(game_entry.status, GameStatus::Active) @ AdPoolError::GameNotActive,
    )]
    pub game_entry: Account<'info, GameEntry>,

    /// The current leader — refunded here. On a self-raise this is the bidder itself.
    /// A stale leader (someone outbid first) fails with the retryable `LeaderChanged`.
    /// CHECK: only lamports are credited; pinned to the stored leader.
    #[account(mut, address = auction.highest_bidder @ AdPoolError::LeaderChanged)]
    pub prev_bidder: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

pub fn handle_bid(ctx: Context<Bid>, amount: u64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let (old_bid, inc_bps, close_ts) = {
        let a = &ctx.accounts.auction;
        (a.highest_bid, a.min_increment_bps, a.close_ts)
    };
    // snapshot, not the current config (design §3)
    require!(now < close_ts, AdPoolError::BiddingClosed);
    require!(
        amount >= min_bid_floor(&Rent::get()?)?,
        AdPoolError::BidBelowFloor
    );
    require!(
        amount >= min_next_bid(old_bid, inc_bps)?,
        AdPoolError::BidTooLow
    );

    let bidder = ctx.accounts.bidder.key();
    let self_raise = bidder == ctx.accounts.auction.highest_bidder;
    let auction_info = ctx.accounts.auction.to_account_info();

    let (refunded_to, refunded) = if self_raise {
        // only the difference moves; nothing to refund
        pay_into_escrow(
            &ctx.accounts.system_program,
            &ctx.accounts.bidder,
            &auction_info,
            amount - old_bid,
        )?;
        (None, 0)
    } else {
        pay_into_escrow(
            &ctx.accounts.system_program,
            &ctx.accounts.bidder,
            &auction_info,
            amount,
        )?;
        // escrow keeps rent: it held >= rent + old_bid (donations may add more) and just
        // received `amount` → after paying old_bid back it holds >= rent + amount
        auction_info.sub_lamports(old_bid)?;
        ctx.accounts.prev_bidder.add_lamports(old_bid)?;
        (Some(ctx.accounts.prev_bidder.key()), old_bid)
    };

    let a = &mut ctx.accounts.auction;
    a.highest_bidder = bidder;
    a.highest_bid = amount;
    a.bid_count = a.bid_count.saturating_add(1);
    a.updated_at = now;

    emit!(BidPlaced {
        game_program: a.game_program,
        epoch: a.epoch,
        slot_index: a.slot_index,
        bidder,
        amount,
        refunded_to,
        refunded,
    });
    Ok(())
}
