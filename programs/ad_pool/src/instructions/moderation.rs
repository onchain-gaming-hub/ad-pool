//! Moderator instructions (design §7). The signer must hold platform-core's `Moderator`
//! role exactly — Admin does not count (pc 03 §2.4 · X4); a revoked role fails closed
//! (the RoleMember account no longer exists). Allowed only in the creative window
//! `[close_ts, epoch_end_ts)`. No Active check (pc 03 §1 rule 3).
//! On-chain checks here are defense in depth, not moderation — the Worker moderates.

use anchor_lang::prelude::*;

use crate::{
    constants::*,
    error::AdPoolError,
    events::{CreativeApproved, CreativeRejected, CreativeRevoked},
    platform_core::{self, accounts::RoleMember, types::Role},
    state::{AdCreative, AdSlotAuction, CreativeStatus},
};

#[derive(Accounts)]
pub struct Moderate<'info> {
    /// Worker key holding the Moderator role; pays the tx fee.
    #[account(mut)]
    pub moderator: Signer<'info>,

    #[account(
        seeds = [PC_ROLE_SEED, &[Role::Moderator as u8], moderator.key().as_ref()],
        bump = moderator_role.bump,
        seeds::program = platform_core::ID,
    )]
    pub moderator_role: Account<'info, RoleMember>,

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
}

fn require_creative_window(auction: &AdSlotAuction, now: i64) -> Result<()> {
    require!(
        auction.close_ts <= now && now < auction.epoch_end_ts,
        AdPoolError::NotInCreativeWindow
    );
    Ok(())
}

/// Byte length in `1..=max` — empty is a shape error, too long is a length error.
fn require_len(value: &str, max: u16) -> Result<()> {
    require!(!value.is_empty(), AdPoolError::InvalidCreativeShape);
    require!(
        value.len() <= max as usize,
        AdPoolError::CreativeFieldTooLong
    );
    Ok(())
}

/// Defense in depth only (the Worker does the real 3-layer check, A14): lowercase
/// `https://` scheme, a non-empty rest, and no ASCII whitespace / control bytes.
fn require_https(url: &str) -> Result<()> {
    require!(
        url.starts_with("https://")
            && url.len() > "https://".len()
            && !url
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control()),
        AdPoolError::InvalidClickUrl
    );
    Ok(())
}

/// Zero every byte from `creative` to the end of the account. Anchor serializes the
/// struct over the old bytes without clearing the tail, so a shorter (or empty)
/// creative would leave the previous strings readable to raw-byte scanners.
fn zero_creative_tail(info: &AccountInfo) -> Result<()> {
    let mut data = info.try_borrow_mut_data()?;
    data[CREATIVE_OFFSET as usize..].fill(0);
    Ok(())
}

// ----------------------------------------------------------- approve_creative

/// First approval of an auction's creative (design §7.1).
/// - full: `image_url` non-empty AND `image_sha256 != 0` → `Approved` (flags must be 0)
/// - text-only: `image_url` empty AND `image_sha256 == 0` → `TextOnly` (A13); non-zero
///   `image_reject_flags` records why the image failed, in the same tx
pub fn handle_approve_creative(
    ctx: Context<Moderate>,
    creative: AdCreative,
    image_reject_flags: u32,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let a = &mut ctx.accounts.auction;
    require_creative_window(a, now)?;
    require!(
        a.creative_status == CreativeStatus::Empty,
        AdPoolError::CreativeAlreadyFinal
    );

    // lengths first: an over-long string would not fit the account (max_len is not
    // enforced when the instruction data is deserialized)
    require!(
        creative.image_url.len() <= AD_IMAGE_URL_MAX_LEN as usize,
        AdPoolError::CreativeFieldTooLong
    );
    require_len(&creative.text, AD_TEXT_MAX_LEN)?;
    require_len(&creative.click_url, AD_CLICK_URL_MAX_LEN)?;
    require_https(&creative.click_url)?;

    let has_url = !creative.image_url.is_empty();
    let has_hash = creative.image_sha256 != [0u8; 32];
    let status = match (has_url, has_hash) {
        (true, true) => {
            require!(image_reject_flags == 0, AdPoolError::InvalidCreativeShape);
            CreativeStatus::Approved
        }
        (false, false) => CreativeStatus::TextOnly,
        _ => return err!(AdPoolError::InvalidCreativeShape),
    };

    if image_reject_flags != 0 {
        a.reject_count = a.reject_count.saturating_add(1);
        a.last_reject_flags = image_reject_flags;
    }
    let image_sha256 = creative.image_sha256;
    a.creative = creative;
    a.creative_status = status;
    a.approved_at = now;
    a.updated_at = now;

    emit!(CreativeApproved {
        moderator: ctx.accounts.moderator.key(),
        game_program: a.game_program,
        epoch: a.epoch,
        slot_index: a.slot_index,
        status,
        image_sha256,
        image_reject_flags,
    });
    Ok(())
}

// --------------------------------------------------------------- approve_image

/// Adds the image to a `TextOnly` creative (A8 → A13 resolved). Text and click_url
/// stay as approved (design §7.2).
pub fn handle_approve_image(
    ctx: Context<Moderate>,
    image_url: String,
    image_sha256: [u8; 32],
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let a = &mut ctx.accounts.auction;
    require_creative_window(a, now)?;
    require!(
        a.creative_status == CreativeStatus::TextOnly,
        AdPoolError::NotTextOnly
    );
    require_len(&image_url, AD_IMAGE_URL_MAX_LEN)?;
    require!(image_sha256 != [0u8; 32], AdPoolError::InvalidImageHash);

    a.creative.image_url = image_url;
    a.creative.image_sha256 = image_sha256;
    a.creative_status = CreativeStatus::Approved;
    a.approved_at = now;
    a.updated_at = now;

    emit!(CreativeApproved {
        moderator: ctx.accounts.moderator.key(),
        game_program: a.game_program,
        epoch: a.epoch,
        slot_index: a.slot_index,
        status: CreativeStatus::Approved,
        image_sha256,
        image_reject_flags: 0,
    });
    Ok(())
}

// ------------------------------------------------------------- reject_creative

/// Records a rejected submission (A8 — status unchanged; D4 → reasons on chain).
/// Not allowed on `Approved`: nothing can be resubmitted there (Q4), so
/// `last_reject_flags` always refers to the live state.
pub fn handle_reject_creative(ctx: Context<Moderate>, flags: u32) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let a = &mut ctx.accounts.auction;
    require_creative_window(a, now)?;
    require!(
        a.creative_status != CreativeStatus::Approved,
        AdPoolError::CreativeAlreadyFinal
    );
    a.reject_count = a.reject_count.saturating_add(1);
    a.last_reject_flags = flags;
    a.updated_at = now;

    emit!(CreativeRejected {
        moderator: ctx.accounts.moderator.key(),
        game_program: a.game_program,
        epoch: a.epoch,
        slot_index: a.slot_index,
        flags,
        reject_count: a.reject_count,
    });
    Ok(())
}

// ------------------------------------------------------------- revoke_creative

/// Takes down an approved (or text-only) creative — e.g. click_url turned malicious
/// after approval (Q9). Back to `Empty` → FE shows the DefaultAd; the winner may
/// resubmit (A8). No funds move (D4).
pub fn handle_revoke_creative(ctx: Context<Moderate>, flags: u32) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let a = &mut ctx.accounts.auction;
    require_creative_window(a, now)?;
    let prev_status = a.creative_status;
    require!(
        prev_status != CreativeStatus::Empty,
        AdPoolError::NothingToRevoke
    );
    zero_creative_tail(&a.to_account_info())?;
    a.creative = AdCreative::default();
    a.creative_status = CreativeStatus::Empty;
    a.approved_at = 0;
    a.reject_count = a.reject_count.saturating_add(1);
    a.last_reject_flags = flags;
    a.updated_at = now;

    emit!(CreativeRevoked {
        moderator: ctx.accounts.moderator.key(),
        game_program: a.game_program,
        epoch: a.epoch,
        slot_index: a.slot_index,
        prev_status,
        flags,
    });
    Ok(())
}
