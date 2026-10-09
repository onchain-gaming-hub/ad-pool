//! Pure helpers shared by instructions (and mirrored by the Worker / FE SDK).
//! All arithmetic is checked; failures map to `AdPoolError::MathOverflow`.

use anchor_lang::prelude::*;

use crate::{
    constants::{AD_SLOT_AUCTION_SIZE, BPS_DENOMINATOR, MAX_FEE_BPS},
    error::AdPoolError,
    state::AdSlotAuction,
};

/// Bytes of an `AdSlotAuction` account (discriminator + fully reserved creative).
pub const AD_SLOT_AUCTION_SPACE: usize = 8 + AdSlotAuction::INIT_SPACE;
const _: () = assert!(AD_SLOT_AUCTION_SPACE == AD_SLOT_AUCTION_SIZE as usize);
const _: () = assert!(
    8 + crate::state::GameAdConfig::INIT_SPACE == crate::constants::GAME_AD_CONFIG_SIZE as usize
);

/// Time phase of an auction — computed, never stored (design §4.1). Half-open ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// `now < close_ts`
    Bidding,
    /// `close_ts <= now < epoch_start_ts`
    CreativeWindow,
    /// `epoch_start_ts <= now < epoch_end_ts`
    Live,
    /// `now >= epoch_end_ts`
    Ended,
}

pub fn phase(now: i64, close_ts: i64, epoch_start_ts: i64, epoch_end_ts: i64) -> Phase {
    if now < close_ts {
        Phase::Bidding
    } else if now < epoch_start_ts {
        Phase::CreativeWindow
    } else if now < epoch_end_ts {
        Phase::Live
    } else {
        Phase::Ended
    }
}

/// `genesis_ts + epoch × epoch_duration_secs` — `epoch` is converted with
/// `i64::try_from` (never `as`, which would wrap).
pub fn epoch_start_ts(genesis_ts: i64, epoch_duration_secs: i64, epoch: u64) -> Result<i64> {
    let epoch = i64::try_from(epoch).map_err(|_| error!(AdPoolError::MathOverflow))?;
    epoch
        .checked_mul(epoch_duration_secs)
        .and_then(|offset| genesis_ts.checked_add(offset))
        .ok_or_else(|| error!(AdPoolError::MathOverflow))
}

/// `(close_ts, epoch_end_ts)` for an epoch starting at `epoch_start_ts`.
pub fn auction_window(
    epoch_start_ts: i64,
    epoch_duration_secs: i64,
    auction_close_lead_secs: i64,
) -> Result<(i64, i64)> {
    let close_ts = epoch_start_ts
        .checked_sub(auction_close_lead_secs)
        .ok_or_else(|| error!(AdPoolError::MathOverflow))?;
    let epoch_end_ts = epoch_start_ts
        .checked_add(epoch_duration_secs)
        .ok_or_else(|| error!(AdPoolError::MathOverflow))?;
    Ok((close_ts, epoch_end_ts))
}

/// `(fee, net)` with `fee = floor(amount × fee_bps / 10,000)` in u128 (pc 03 §3).
/// Rounding goes to the recipient, never to the platform.
/// Only guards `fee_bps <= 10,000` (what `fee <= amount` needs): the `MAX_FEE_BPS` cap is
/// checked once at `open_auction` before snapshotting, so `settle` of an existing auction
/// can never be bricked by a later change of the cap.
pub fn fee_split(amount: u64, fee_bps: u16) -> Result<(u64, u64)> {
    require!(fee_bps <= BPS_DENOMINATOR, AdPoolError::FeeTooHigh);
    let fee = (amount as u128)
        .checked_mul(fee_bps as u128)
        .ok_or_else(|| error!(AdPoolError::MathOverflow))?
        / BPS_DENOMINATOR as u128;
    let fee = u64::try_from(fee).map_err(|_| error!(AdPoolError::MathOverflow))?;
    // fee <= amount because fee_bps <= BPS_DENOMINATOR.
    Ok((fee, amount - fee))
}

/// Smallest bid that beats `highest_bid`:
/// `highest_bid + max(1, ceil(highest_bid × min_increment_bps / 10,000))`.
pub fn min_next_bid(highest_bid: u64, min_increment_bps: u16) -> Result<u64> {
    let denom = BPS_DENOMINATOR as u128;
    let step = (highest_bid as u128)
        .checked_mul(min_increment_bps as u128)
        .ok_or_else(|| error!(AdPoolError::MathOverflow))?
        .div_ceil(denom)
        .max(1);
    let next = (highest_bid as u128)
        .checked_add(step)
        .ok_or_else(|| error!(AdPoolError::MathOverflow))?;
    u64::try_from(next).map_err(|_| error!(AdPoolError::MathOverflow))
}

/// Floor for `min_bid` and for every bid (design §5.3 · maintainer Q6):
/// `max(rent(AdSlotAuction), ceil(rent(0) × 10,000 / (10,000 − MAX_FEE_BPS)))`.
/// The first term makes an ad never cheaper than its on-chain storage; the second keeps
/// every refund and every settle payout ≥ rent(0), so crediting a drained (0-lamport)
/// wallet can never fail. With current rent the first term dominates.
pub fn min_bid_floor(rent: &Rent) -> Result<u64> {
    let storage = rent.minimum_balance(AD_SLOT_AUCTION_SPACE);
    let zero = rent.minimum_balance(0) as u128;
    let payout_safe = zero
        .checked_mul(BPS_DENOMINATOR as u128)
        .ok_or_else(|| error!(AdPoolError::MathOverflow))?
        .div_ceil((BPS_DENOMINATOR - MAX_FEE_BPS) as u128);
    let payout_safe = u64::try_from(payout_safe).map_err(|_| error!(AdPoolError::MathOverflow))?;
    Ok(storage.max(payout_safe))
}

#[cfg(test)]
mod tests {
    use super::*;

    // `Rent`'s fields differ between solana-rent 3.x and 4.x, so tests only use
    // `Rent::default()` + `minimum_balance` (the same call the program makes on chain).

    #[test]
    fn auction_space_matches_design() {
        assert_eq!(AD_SLOT_AUCTION_SPACE, 779);
    }

    #[test]
    fn floor_is_rent_of_auction_account() {
        let r = Rent::default();
        let floor = min_bid_floor(&r).unwrap();
        assert_eq!(floor, r.minimum_balance(AD_SLOT_AUCTION_SPACE));
        // rent is linear in (128 + size): storage term must dominate the payout term
        assert!(floor > r.minimum_balance(0) * 10_000 / 8_000);
    }

    #[test]
    fn floor_keeps_payout_above_rent_zero_at_max_fee() {
        let r = Rent::default();
        let floor = min_bid_floor(&r).unwrap();
        let (_, net) = fee_split(floor, MAX_FEE_BPS).unwrap();
        assert!(net >= r.minimum_balance(0));
    }

    #[test]
    fn phase_boundaries_are_half_open() {
        let (c, s, e) = (100, 200, 300);
        assert_eq!(phase(99, c, s, e), Phase::Bidding);
        assert_eq!(phase(100, c, s, e), Phase::CreativeWindow);
        assert_eq!(phase(199, c, s, e), Phase::CreativeWindow);
        assert_eq!(phase(200, c, s, e), Phase::Live);
        assert_eq!(phase(299, c, s, e), Phase::Live);
        assert_eq!(phase(300, c, s, e), Phase::Ended);
    }

    #[test]
    fn epoch_start_and_window() {
        let day = 86_400;
        let genesis = 1_790_000_000 - 1_790_000_000 % day;
        let start = epoch_start_ts(genesis, day, 10).unwrap();
        assert_eq!(start, genesis + 10 * day);
        let (close, end) = auction_window(start, day, 7 * day).unwrap();
        assert_eq!(close, start - 7 * day);
        assert_eq!(end, start + day);
    }

    #[test]
    fn epoch_overflow_is_an_error_not_a_wrap() {
        assert!(epoch_start_ts(0, 86_400, u64::MAX).is_err());
        assert!(epoch_start_ts(0, 86_400, i64::MAX as u64).is_err());
        assert!(epoch_start_ts(i64::MAX, 86_400, 1).is_err());
        assert!(auction_window(i64::MIN, 86_400, 1).is_err());
        assert!(auction_window(i64::MAX, 1, 0).is_err());
    }

    #[test]
    fn fee_split_floors_toward_recipient() {
        assert_eq!(
            fee_split(1_000_000_000, 100).unwrap(),
            (10_000_000, 990_000_000)
        );
        assert_eq!(fee_split(199, 100).unwrap(), (1, 198));
        assert_eq!(fee_split(99, 100).unwrap(), (0, 99));
        assert_eq!(fee_split(u64::MAX, MAX_FEE_BPS).unwrap().0, u64::MAX / 5);
        assert_eq!(fee_split(100, BPS_DENOMINATOR).unwrap(), (100, 0));
        assert!(fee_split(1, BPS_DENOMINATOR + 1).is_err());
    }

    #[test]
    fn min_next_bid_rounds_up_and_is_strictly_greater() {
        assert_eq!(min_next_bid(1_000, 500).unwrap(), 1_050);
        assert_eq!(min_next_bid(1_001, 500).unwrap(), 1_052); // ceil(50.05) = 51
        assert_eq!(min_next_bid(10, 1).unwrap(), 11); // step max(1, ceil(0.001)) = 1
        assert_eq!(min_next_bid(0, 10_000).unwrap(), 1);
        assert!(min_next_bid(u64::MAX, 1).is_err());
    }
}
