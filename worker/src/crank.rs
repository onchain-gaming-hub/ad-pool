//! W3 crank (Cron Trigger): what to do with every `AdSlotAuction` this run. Pure (tested
//! natively); the Wasm side lists auctions (`getProgramAccounts`), sends the txs and runs the
//! URL re-checks. Design §6.1: the crank settles + closes **every** auction, whoever paid rent.
//! - `settle` after `epoch_start_ts` · `close_auction` after `epoch_end_ts` (one tx when both
//!   are due) — signed by the **crank key** (no role, fee SOL only), never the Moderator key
//! - re-check `click_url` of `TextOnly`/`Approved` auctions inside `[close_ts, epoch_end_ts)`
//!   (the only window `revoke_creative` accepts) → dangerous → `revoke_creative` (Moderator key)

use solana_pubkey::Pubkey;

use crate::{
    accounts::{AdSlotAuction, CreativeStatus},
    phase,
};

/// A settle/close still not done this long after it became due is reported.
pub const STALE_SECS: i64 = 6 * 3_600;
/// Bounds one run's fees and wall time; the rest waits for the next run.
pub const MAX_TXS_PER_RUN: usize = 10;
pub const MAX_RECHECKS_PER_RUN: usize = 10;
/// Cron period (wrangler.toml `crons = ["*/10 * * * *"]`) — used to rotate re-checks.
pub const RUN_PERIOD_SECS: i64 = 600;
/// Worst-case subrequests of one settle/close tx: GameEntry read + blockhash + send.
pub const TX_COST: i64 = 3;
/// Worst-case subrequests of one URL re-check: per hop (cache get + Web Risk + cache put +
/// fetch) × (redirects + 1), then blockhash + send of a revoke.
pub const RECHECK_COST: i64 = 4 * (crate::urlcheck::MAX_REDIRECTS as i64 + 1) + 2;

/// Spend `cost` from the per-run subrequest budget, or refuse (the item waits for a later run).
pub fn take(budget: &mut i64, cost: i64) -> bool {
    if *budget < cost {
        return false;
    }
    *budget -= cost;
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Due {
    Settle,
    Close,
    SettleAndClose,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stale {
    pub auction: Pubkey,
    pub what: Due,
    pub overdue_secs: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// earliest-due first, at most `MAX_TXS_PER_RUN`
    pub txs: Vec<(Pubkey, Due)>,
    /// at most `MAX_RECHECKS_PER_RUN`, rotated across runs when there are more
    pub rechecks: Vec<Pubkey>,
    pub stale: Vec<Stale>,
    /// due txs left for the next run (cap reached)
    pub deferred: usize,
}

pub fn due(now: i64, a: &AdSlotAuction) -> Option<Due> {
    let start = now >= a.epoch_start_ts;
    let end = now >= a.epoch_end_ts;
    match (a.settled, start, end) {
        (false, _, true) => Some(Due::SettleAndClose),
        (false, true, false) => Some(Due::Settle),
        (true, _, true) => Some(Due::Close),
        _ => None,
    }
}

/// When the pending action became due (for ordering and staleness).
fn due_since(a: &AdSlotAuction, d: Due) -> i64 {
    match d {
        Due::Settle | Due::SettleAndClose => a.epoch_start_ts,
        Due::Close => a.epoch_end_ts,
    }
}

pub fn needs_recheck(now: i64, a: &AdSlotAuction) -> bool {
    matches!(
        a.creative_status,
        CreativeStatus::TextOnly | CreativeStatus::Approved
    ) && phase::in_creative_window(now, a)
        && !a.creative.click_url.is_empty()
}

/// At most `cap` items; when there are more, each run starts where the previous one stopped.
fn rotate<T: Copy>(v: Vec<T>, cap: usize, now: i64) -> Vec<T> {
    if v.len() <= cap {
        return v;
    }
    let start = ((now / RUN_PERIOD_SECS) as usize * cap) % v.len();
    (0..cap).map(|i| v[(start + i) % v.len()]).collect()
}

pub fn plan(now: i64, auctions: &[(Pubkey, AdSlotAuction)]) -> Plan {
    let mut due_list: Vec<(i64, Pubkey, Due)> = auctions
        .iter()
        .filter_map(|(k, a)| due(now, a).map(|d| (due_since(a, d), *k, d)))
        .collect();
    due_list.sort_by_key(|(since, k, _)| (*since, *k));
    let stale = due_list
        .iter()
        .filter(|(since, _, _)| now - since > STALE_SECS)
        .map(|(since, k, d)| Stale {
            auction: *k,
            what: *d,
            overdue_secs: now - since,
        })
        .collect();
    let deferred = due_list.len().saturating_sub(MAX_TXS_PER_RUN);
    let all: Vec<(Pubkey, Due)> = due_list.into_iter().map(|(_, k, d)| (k, d)).collect();
    // earliest-due first; over the cap, rotate like the re-checks so auctions whose tx keeps
    // failing (they stay earliest-due) cannot starve the rest
    let txs = rotate(all, MAX_TXS_PER_RUN, now);

    let mut re: Vec<Pubkey> = auctions
        .iter()
        .filter(|(_, a)| needs_recheck(now, a))
        .map(|(k, _)| *k)
        .collect();
    re.sort();
    let rechecks = rotate(re, MAX_RECHECKS_PER_RUN, now);
    Plan {
        txs,
        rechecks,
        stale,
        deferred,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::Creative;

    const T: i64 = 1_800_000_000;

    fn auction(settled: bool, status: CreativeStatus, start: i64) -> AdSlotAuction {
        AdSlotAuction {
            bump: 255,
            creative_status: status,
            settled,
            slot_index: 0,
            game_program: Pubkey::new_unique(),
            epoch: 1,
            width: 1,
            height: 1,
            close_ts: start - 7 * 86_400,
            epoch_start_ts: start,
            epoch_end_ts: start + 86_400,
            fee_bps: 100,
            min_increment_bps: 100,
            rent_payer: Pubkey::new_unique(),
            highest_bidder: Pubkey::new_unique(),
            highest_bid: 1,
            bid_count: 1,
            reject_count: 0,
            last_reject_flags: 0,
            approved_at: 0,
            updated_at: 0,
            creative: Creative {
                click_url: if status == CreativeStatus::Empty {
                    String::new()
                } else {
                    "https://example.com/".into()
                },
                ..Creative::default()
            },
        }
    }

    #[test]
    fn budget_is_never_overspent() {
        // Free plan (50) − cluster check − getProgramAccounts
        let mut b = 45 - 2;
        assert!(take(&mut b, RECHECK_COST));
        assert_eq!(b, 43 - RECHECK_COST);
        let mut n = 0;
        while take(&mut b, TX_COST) {
            n += 1;
        }
        assert!((0..TX_COST).contains(&b));
        assert_eq!(n, (43 - RECHECK_COST) / TX_COST);
        assert!(!take(&mut b, RECHECK_COST));
    }

    #[test]
    fn due_follows_the_clock_and_settled_flag() {
        let a = auction(false, CreativeStatus::Empty, T);
        assert_eq!(due(T - 1, &a), None);
        assert_eq!(due(T, &a), Some(Due::Settle));
        assert_eq!(due(T + 86_400, &a), Some(Due::SettleAndClose));
        let s = auction(true, CreativeStatus::Approved, T);
        assert_eq!(due(T + 10, &s), None);
        assert_eq!(due(T + 86_400, &s), Some(Due::Close));
    }

    #[test]
    fn plan_orders_caps_and_reports_stale() {
        let mut list = vec![];
        for i in 0..(MAX_TXS_PER_RUN as i64 + 3) {
            list.push((
                Pubkey::new_unique(),
                auction(false, CreativeStatus::Empty, T - 100 * i),
            ));
        }
        let old = Pubkey::new_unique();
        list.push((
            old,
            auction(false, CreativeStatus::Empty, T - STALE_SECS - 10),
        ));
        let p = plan(T, &list);
        assert_eq!(p.txs.len(), MAX_TXS_PER_RUN);
        assert_eq!(p.deferred, 4);
        // the most overdue is reported; over the cap the list rotates across runs so a
        // stuck auction cannot starve the rest
        assert!(p.txs.iter().all(|(k, _)| list.iter().any(|(x, _)| x == k)));
        let mut seen = std::collections::HashSet::new();
        for run in 0..3 {
            seen.extend(
                plan(T + run * RUN_PERIOD_SECS, &list)
                    .txs
                    .into_iter()
                    .map(|(k, _)| k),
            );
        }
        assert_eq!(seen.len(), list.len());
        assert!(seen.contains(&old));
        assert_eq!(p.stale.len(), 1);
        assert_eq!(p.stale[0].auction, old);
        // not yet due → nothing
        let fresh = vec![(
            Pubkey::new_unique(),
            auction(false, CreativeStatus::Empty, T + 1),
        )];
        assert_eq!(plan(T, &fresh), Plan::default());
    }

    #[test]
    fn rechecks_only_live_approved_or_text_only_inside_the_revoke_window() {
        let ok = auction(false, CreativeStatus::Approved, T + 3_600); // in window (close passed)
        let txt = auction(false, CreativeStatus::TextOnly, T + 3_600);
        let empty = auction(false, CreativeStatus::Empty, T + 3_600);
        let ended = auction(true, CreativeStatus::Approved, T - 2 * 86_400);
        let bidding = auction(false, CreativeStatus::Approved, T + 30 * 86_400); // before close_ts
        let keys: Vec<Pubkey> = (0..5).map(|_| Pubkey::new_unique()).collect();
        let list: Vec<(Pubkey, AdSlotAuction)> = keys
            .iter()
            .copied()
            .zip([ok, txt, empty, ended, bidding])
            .collect();
        let mut want = vec![keys[0], keys[1]];
        want.sort();
        assert_eq!(plan(T, &list).rechecks, want);
    }

    #[test]
    fn rechecks_rotate_over_runs_and_cover_everyone() {
        let list: Vec<(Pubkey, AdSlotAuction)> = (0..25)
            .map(|_| {
                (
                    Pubkey::new_unique(),
                    auction(false, CreativeStatus::Approved, T + 3_600),
                )
            })
            .collect();
        let mut seen = std::collections::HashSet::new();
        for run in 0..3 {
            let p = plan(T + run * RUN_PERIOD_SECS, &list);
            assert_eq!(p.rechecks.len(), MAX_RECHECKS_PER_RUN);
            seen.extend(p.rechecks);
        }
        assert_eq!(seen.len(), 25);
    }
}
