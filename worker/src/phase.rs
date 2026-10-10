//! Time phase of an auction — mirrors `ad_pool::math::phase` (design §4.1, half-open).

use crate::accounts::AdSlotAuction;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Phase {
    Bidding,
    CreativeWindow,
    Live,
    Ended,
}

pub fn phase(now: i64, a: &AdSlotAuction) -> Phase {
    if now < a.close_ts {
        Phase::Bidding
    } else if now < a.epoch_start_ts {
        Phase::CreativeWindow
    } else if now < a.epoch_end_ts {
        Phase::Live
    } else {
        Phase::Ended
    }
}

/// Moderator instructions are accepted on chain only in `[close_ts, epoch_end_ts)`.
pub fn in_creative_window(now: i64, a: &AdSlotAuction) -> bool {
    a.close_ts <= now && now < a.epoch_end_ts
}
