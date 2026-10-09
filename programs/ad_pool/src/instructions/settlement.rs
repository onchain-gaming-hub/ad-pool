//! Settlement (design §6). Both are permissionless cranks with no signer in the ix.
//! `settle`: after the epoch starts, pay the winning bid out of escrow — fee to
//! platform-core's Treasury, the rest to `GameEntry.owner` (read at settle time).
//! No refund ever (D4). No Active check (pc 03 §1 rule 3 — Suspend must not lock funds).
//! `close_auction`: pure close after the epoch ends and settle ran; everything left
//! (rent + any donation) goes back to `rent_payer` (D11). Fixed 2-account shape so a
//! generic sweeper can call it.

use anchor_lang::prelude::*;

use crate::{
    constants::*,
    error::AdPoolError,
    events::{AuctionClosed, Settled},
    math::fee_split,
    platform_core::{
        self,
        accounts::{GameEntry, PlatformConfig},
    },
    state::AdSlotAuction,
};

// ------------------------------------------------------------------- settle

#[derive(Accounts)]
pub struct Settle<'info> {
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

    /// Seeds come from the auction — never from an argument — so the GameEntry (and its
    /// owner, the payee) of another game cannot be substituted. Status is NOT checked.
    #[account(
        seeds = [PC_GAME_SEED, auction.game_program.as_ref()],
        bump = game_entry.bump,
        seeds::program = platform_core::ID,
    )]
    pub game_entry: Account<'info, GameEntry>,

    /// Revenue recipient = `GameEntry.owner` at settle time. UncheckedAccount (Q12): a
    /// non-system-owned owner (multisig PDA…) must not make settle — and rent — stuck.
    /// CHECK: only credited; pinned to GameEntry.owner.
    #[account(mut, address = game_entry.owner @ AdPoolError::NotGameOwner)]
    pub game_owner: UncheckedAccount<'info>,

    /// Needed only for `treasury_bump`.
    #[account(
        seeds = [PC_CONFIG_SEED],
        bump = config.bump,
        seeds::program = platform_core::ID,
    )]
    pub config: Account<'info, PlatformConfig>,

    /// platform-core Treasury PDA — must be pinned (pc 03 §2.6), else anyone could
    /// redirect the fee.
    /// CHECK: only credited; seeds + seeds::program pin it.
    #[account(
        mut,
        seeds = [PC_TREASURY_SEED],
        bump = config.treasury_bump,
        seeds::program = platform_core::ID,
    )]
    pub treasury: UncheckedAccount<'info>,
}

pub fn handle_settle(ctx: Context<Settle>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let (highest_bid, fee_bps, winner) = {
        let a = &ctx.accounts.auction;
        require!(now >= a.epoch_start_ts, AdPoolError::EpochNotStarted);
        require!(!a.settled, AdPoolError::AlreadySettled);
        (a.highest_bid, a.fee_bps, a.highest_bidder)
    };
    // Pay from `highest_bid` only — never `lamports − rent` (donations stay for rent_payer).
    let (fee, net) = fee_split(highest_bid, fee_bps)?;
    ctx.accounts
        .auction
        .to_account_info()
        .sub_lamports(highest_bid)?;
    ctx.accounts.treasury.add_lamports(fee)?;
    ctx.accounts.game_owner.add_lamports(net)?;

    let a = &mut ctx.accounts.auction;
    a.settled = true;
    a.updated_at = now;
    emit!(Settled {
        game_program: a.game_program,
        epoch: a.epoch,
        slot_index: a.slot_index,
        winner,
        amount: highest_bid,
        fee,
        net,
        game_owner: ctx.accounts.game_owner.key(),
    });
    Ok(())
}

// ------------------------------------------------------------- close_auction

#[derive(Accounts)]
pub struct CloseAuction<'info> {
    #[account(
        mut,
        close = rent_payer,
        has_one = rent_payer @ AdPoolError::RentPayerMismatch,
        seeds = [
            AD_SLOT_SEED,
            auction.game_program.as_ref(),
            &auction.epoch.to_le_bytes(),
            &[auction.slot_index],
        ],
        bump = auction.bump,
    )]
    pub auction: Account<'info, AdSlotAuction>,

    /// Gets everything left in the escrow. Not a signer — funds can only go here, so
    /// anyone may close.
    /// CHECK: pinned by `has_one`; only credited.
    #[account(mut)]
    pub rent_payer: UncheckedAccount<'info>,
}

pub fn handle_close_auction(ctx: Context<CloseAuction>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let a = &ctx.accounts.auction;
    require!(now >= a.epoch_end_ts, AdPoolError::EpochNotEnded);
    require!(a.settled, AdPoolError::NotSettled);
    emit!(AuctionClosed {
        game_program: a.game_program,
        epoch: a.epoch,
        slot_index: a.slot_index,
        rent_payer: a.rent_payer,
        refunded: a.to_account_info().lamports(),
    });
    Ok(())
}
