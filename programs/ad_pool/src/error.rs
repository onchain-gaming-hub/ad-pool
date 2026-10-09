use anchor_lang::prelude::*;

/// ⚠️ Append only — codes are part of the interface (FE retries on some of them).
/// Order = design §12.
#[error_code]
pub enum AdPoolError {
    #[msg("Game is not Active in platform-core")]
    GameNotActive,
    #[msg("Signer / account is not GameEntry.owner")]
    NotGameOwner,
    #[msg(
        "epoch_duration_secs must be a positive multiple of 86,400 and <= MAX_EPOCH_DURATION_SECS"
    )]
    InvalidEpochDuration,
    #[msg("genesis_ts must be a multiple of 86,400 (00:00 UTC)")]
    InvalidGenesis,
    #[msg("auction_close_lead_secs must be >= 7 days")]
    CloseLeadTooShort,
    #[msg("bid_horizon_epochs × epoch_duration_secs must be >= close lead + one epoch")]
    HorizonTooShort,
    #[msg("bid_horizon_epochs × epoch_duration_secs exceeds MAX_HORIZON_SECS")]
    HorizonTooLong,
    #[msg("platform-core ad_fee_bps exceeds MAX_FEE_BPS")]
    FeeTooHigh,
    #[msg("min_bid is below the floor (rent of an AdSlotAuction)")]
    MinBidBelowFloor,
    #[msg("Bid is below the floor (rent of an AdSlotAuction)")]
    BidBelowFloor,
    #[msg("min_increment_bps must be in 1..=10,000")]
    InvalidIncrement,
    #[msg("Invalid slot (index out of range or zero width/height)")]
    InvalidSlot,
    #[msg("Slot is not active")]
    SlotInactive,
    #[msg("Too many slots (max 8)")]
    TooManySlots,
    #[msg("Bidding is closed for this auction")]
    BiddingClosed,
    #[msg("Epoch is beyond the bid horizon")]
    BeyondHorizon,
    #[msg("Bid is below the minimum (min_bid or minimum increment)")]
    BidTooLow,
    #[msg("Leader changed — re-read the auction and rebuild the transaction")]
    LeaderChanged,
    #[msg("Not inside the creative window [close_ts, epoch_end_ts)")]
    NotInCreativeWindow,
    #[msg("Creative is already final for this action")]
    CreativeAlreadyFinal,
    #[msg("Creative status is not TextOnly")]
    NotTextOnly,
    #[msg("Nothing to revoke")]
    NothingToRevoke,
    #[msg("Invalid creative shape")]
    InvalidCreativeShape,
    #[msg("Creative field too long (bytes)")]
    CreativeFieldTooLong,
    #[msg("click_url must start with https://")]
    InvalidClickUrl,
    #[msg("image_sha256 must not be all zeros")]
    InvalidImageHash,
    #[msg("Epoch has not started")]
    EpochNotStarted,
    #[msg("Already settled")]
    AlreadySettled,
    #[msg("Epoch has not ended")]
    EpochNotEnded,
    #[msg("Not settled yet")]
    NotSettled,
    #[msg("rent_payer does not match the auction")]
    RentPayerMismatch,
    #[msg("Math overflow")]
    MathOverflow,
}
