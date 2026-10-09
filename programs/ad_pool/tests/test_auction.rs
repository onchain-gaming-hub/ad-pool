//! P2 — open_auction + bid (design §4.2 · §5 · §14 "เวลา / refund / open race / rent / floor /
//! TTL / Suspended / overflow").
#![allow(clippy::result_large_err)]

mod common;

use {
    ad_pool::{
        constants::*,
        error::AdPoolError as E,
        events::{AuctionOpened, BidPlaced},
        platform_core::types::GameStatus,
        state::{AdCreative, CreativeStatus},
    },
    anchor_lang::prelude::Pubkey,
    common::*,
    solana_account::Account,
    solana_keypair::Keypair,
    solana_signer::Signer,
};

const E8: u64 = FIRST_OPEN_EPOCH;

fn start_of(epoch: u64) -> i64 {
    T0 + epoch as i64 * DAY
}

fn open(
    svm: &mut litesvm::LiteSVM,
    g: &Game,
    who: &Keypair,
    epoch: u64,
    slot: u8,
    amount: u64,
) -> litesvm::types::TransactionResult {
    send(
        svm,
        &[ix_open_auction(
            &who.pubkey(),
            &g.program,
            epoch,
            slot,
            amount,
        )],
        &[who],
    )
}

fn bid(
    svm: &mut litesvm::LiteSVM,
    g: &Game,
    who: &Keypair,
    epoch: u64,
    slot: u8,
    amount: u64,
) -> litesvm::types::TransactionResult {
    let ix = ix_bid(svm, &who.pubkey(), &g.program, epoch, slot, amount);
    send(svm, &[ix], &[who])
}

// ------------------------------------------------------------- open_auction

#[test]
fn open_snapshots_everything_and_escrows_the_bid() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let alice = funded(&mut svm);
    let amount = a.min_bid + 123;
    let before = lamports(&svm, &alice.pubkey());
    let res = open(&mut svm, &g, &alice, E8, 1, amount);
    assert_ok(&res);

    let addr = ad_slot_pda(&g.program, E8, 1);
    let au = auction_of(&svm, &g.program, E8, 1);
    assert_eq!(au.version, ACCOUNT_VERSION);
    assert_eq!(
        au.bump,
        Pubkey::find_program_address(
            &[AD_SLOT_SEED, g.program.as_ref(), &E8.to_le_bytes(), &[1]],
            &ad_pool::id()
        )
        .1
    );
    assert_eq!(au.creative_status, CreativeStatus::Empty);
    assert!(!au.settled);
    assert_eq!(
        (au.slot_index, au.game_program, au.epoch),
        (1, g.program, E8)
    );
    assert_eq!((au.width, au.height), (300, 250));
    assert_eq!(au.epoch_start_ts, start_of(E8));
    assert_eq!(au.close_ts, start_of(E8) - 7 * DAY);
    assert_eq!(au.epoch_end_ts, start_of(E8) + DAY);
    assert_eq!(au.fee_bps, 100);
    assert_eq!(au.min_increment_bps, 500);
    assert_eq!(
        (
            au.rent_payer,
            au.highest_bidder,
            au.highest_bid,
            au.bid_count
        ),
        (alice.pubkey(), alice.pubkey(), amount, 1)
    );
    assert_eq!(
        (
            au.reject_count,
            au.last_reject_flags,
            au.approved_at,
            au.updated_at
        ),
        (0, 0, 0, T0)
    );
    assert_eq!(au.creative, AdCreative::default());

    let rent = rent_min(&svm, AD_SLOT_AUCTION_SIZE as usize);
    let acct = svm.get_account(&addr).unwrap();
    assert_eq!(acct.data.len(), AD_SLOT_AUCTION_SIZE as usize);
    assert_eq!(acct.lamports, rent + amount, "escrow = rent + bid");
    assert_eq!(
        before - lamports(&svm, &alice.pubkey()),
        amount + rent + base_fee(1),
        "bidder pays bid + rent + fee"
    );

    let opened: Vec<AuctionOpened> = events(&res);
    assert_eq!(opened.len(), 1);
    let o = &opened[0];
    assert_eq!(
        (
            o.game_program,
            o.epoch,
            o.slot_index,
            o.rent_payer,
            o.fee_bps
        ),
        (g.program, E8, 1, alice.pubkey(), 100)
    );
    assert_eq!(
        (o.close_ts, o.epoch_start_ts, o.epoch_end_ts),
        (au.close_ts, au.epoch_start_ts, au.epoch_end_ts)
    );
    let bids: Vec<BidPlaced> = events(&res);
    assert_eq!(bids.len(), 1);
    assert_eq!(
        (
            bids[0].bidder,
            bids[0].amount,
            bids[0].refunded_to,
            bids[0].refunded
        ),
        (alice.pubkey(), amount, None, 0)
    );
}

#[test]
fn open_time_window_is_half_open() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let alice = funded(&mut svm);
    // epoch 8 closes at T0 + 1 d
    let close = start_of(E8) - 7 * DAY;
    set_time(&mut svm, close - 1);
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
    set_time(&mut svm, close);
    assert_custom_err(
        &open(&mut svm, &g, &alice, E8, 1, a.min_bid),
        code(E::BiddingClosed),
    );
    // an epoch that already started
    set_time(&mut svm, T0);
    assert_custom_err(
        &open(&mut svm, &g, &alice, 0, 0, a.min_bid),
        code(E::BiddingClosed),
    );
    assert_custom_err(
        &open(&mut svm, &g, &alice, 7, 0, a.min_bid),
        code(E::BiddingClosed),
    );
}

#[test]
fn open_respects_horizon() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let alice = funded(&mut svm);
    assert_ok(&open(&mut svm, &g, &alice, LAST_OPEN_EPOCH, 0, a.min_bid));
    assert_custom_err(
        &open(&mut svm, &g, &alice, LAST_OPEN_EPOCH + 1, 0, a.min_bid),
        code(E::BeyondHorizon),
    );
}

#[test]
fn open_epoch_overflow_is_math_overflow() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let alice = funded(&mut svm);
    assert_custom_err(
        &open(&mut svm, &g, &alice, u64::MAX, 0, a.min_bid),
        code(E::MathOverflow),
    );
    assert_custom_err(
        &open(&mut svm, &g, &alice, i64::MAX as u64, 0, a.min_bid),
        code(E::MathOverflow),
    );
}

#[test]
fn open_checks_slot_index_before_active() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let alice = funded(&mut svm);
    // slot 2..7 exist in the array but are unused (active = false) → InvalidSlot
    assert_custom_err(
        &open(&mut svm, &g, &alice, E8, 2, a.min_bid),
        code(E::InvalidSlot),
    );
    assert_custom_err(
        &open(&mut svm, &g, &alice, E8, 200, a.min_bid),
        code(E::InvalidSlot),
    );
    assert_ok(&send(
        &mut svm,
        &[ix_set_slot_active(&g.owner.pubkey(), &g.program, 0, false)],
        &[&g.owner],
    ));
    assert_custom_err(
        &open(&mut svm, &g, &alice, E8, 0, a.min_bid),
        code(E::SlotInactive),
    );
}

#[test]
fn inactive_slot_blocks_new_auctions_but_existing_ones_continue() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let (alice, bob) = (funded(&mut svm), funded(&mut svm));
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
    assert_ok(&send(
        &mut svm,
        &[ix_set_slot_active(&g.owner.pubkey(), &g.program, 0, false)],
        &[&g.owner],
    ));
    assert_custom_err(
        &open(&mut svm, &g, &alice, E8 + 1, 0, a.min_bid),
        code(E::SlotInactive),
    );
    assert_ok(&bid(&mut svm, &g, &bob, E8, 0, a.min_bid * 2));
}

#[test]
fn open_amount_floor_and_min_bid() {
    let mut svm = setup();
    let (g, _) = new_configured_game(&mut svm);
    let alice = funded(&mut svm);
    let f = floor(&svm);
    assert_custom_err(
        &open(&mut svm, &g, &alice, E8, 0, f - 1),
        code(E::BidBelowFloor),
    );
    // raise min_bid above the floor → below min_bid is BidTooLow
    assert_ok(&send(
        &mut svm,
        &[ix_update_ad_config(
            &g.owner.pubkey(),
            &g.program,
            None,
            Some(f * 3),
            None,
            None,
        )],
        &[&g.owner],
    ));
    assert_custom_err(
        &open(&mut svm, &g, &alice, E8, 0, f * 3 - 1),
        code(E::BidTooLow),
    );
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, f * 3));
}

#[test]
fn open_race_second_open_fails_without_overwriting() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let (alice, bob) = (funded(&mut svm), funded(&mut svm));
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
    let addr = ad_slot_pda(&g.program, E8, 0);
    let before = svm.get_account(&addr).unwrap();
    let res = open(&mut svm, &g, &bob, E8, 0, a.min_bid * 5);
    assert_custom_err(&res, 0); // System AccountAlreadyInUse — FE retries with `bid`
    let after = svm.get_account(&addr).unwrap();
    assert_eq!((after.data, after.lamports), (before.data, before.lamports));
}

#[test]
fn open_works_on_prefunded_pda_and_bidder_pays_only_the_shortfall() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let alice = funded(&mut svm);
    let addr = ad_slot_pda(&g.program, E8, 0);
    let donation = 1_000;
    svm.set_account(
        addr,
        Account {
            lamports: donation,
            data: vec![],
            owner: anchor_lang::solana_program::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap();
    let before = lamports(&svm, &alice.pubkey());
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
    let rent = rent_min(&svm, AD_SLOT_AUCTION_SIZE as usize);
    assert_eq!(
        before - lamports(&svm, &alice.pubkey()),
        a.min_bid + rent - donation + base_fee(1)
    );
    assert_eq!(lamports(&svm, &addr), rent + a.min_bid);
}

#[test]
fn open_rejects_fee_above_cap_and_snapshots_fee() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let alice = funded(&mut svm);
    put_pc_config(&mut svm, MAX_FEE_BPS + 1);
    assert_custom_err(
        &open(&mut svm, &g, &alice, E8, 0, a.min_bid),
        code(E::FeeTooHigh),
    );
    put_pc_config(&mut svm, MAX_FEE_BPS);
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
    // later fee change in platform-core does not touch the open auction
    put_pc_config(&mut svm, 50);
    assert_ok(&open(&mut svm, &g, &alice, E8, 1, a.min_bid));
    assert_eq!(auction_of(&svm, &g.program, E8, 0).fee_bps, MAX_FEE_BPS);
    assert_eq!(auction_of(&svm, &g.program, E8, 1).fee_bps, 50);
}

#[test]
fn open_and_bid_require_active_game() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let (alice, bob) = (funded(&mut svm), funded(&mut svm));
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
    put_game_entry(
        &mut svm,
        &g.program,
        &g.owner.pubkey(),
        GameStatus::Suspended,
    );
    assert_custom_err(
        &open(&mut svm, &g, &alice, E8, 1, a.min_bid),
        code(E::GameNotActive),
    );
    assert_custom_err(
        &bid(&mut svm, &g, &bob, E8, 0, a.min_bid * 2),
        code(E::GameNotActive),
    );
    // reactivated → bidding works again
    put_game_entry(&mut svm, &g.program, &g.owner.pubkey(), GameStatus::Active);
    assert_ok(&bid(&mut svm, &g, &bob, E8, 0, a.min_bid * 2));
}

#[test]
fn open_without_config_or_with_fake_pc_accounts_fails() {
    let mut svm = setup();
    let g = new_game(&mut svm); // registered, but no ad config
    let alice = funded(&mut svm);
    let f = floor(&svm);
    assert_custom_err(&open(&mut svm, &g, &alice, E8, 0, f), 3012);
    let (g2, _) = new_configured_game(&mut svm);
    put_fake_game_entry(
        &mut svm,
        &g2.program,
        &g2.owner.pubkey(),
        Pubkey::new_unique(),
    );
    assert_custom_err(&open(&mut svm, &g2, &alice, E8, 0, f), 3007);
}

// ---------------------------------------------------------------------- bid

#[test]
fn outbid_refunds_previous_leader_in_full() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let (alice, bob) = (funded(&mut svm), funded(&mut svm));
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
    let addr = ad_slot_pda(&g.program, E8, 0);
    let rent = rent_min(&svm, AD_SLOT_AUCTION_SIZE as usize);
    let (alice_before, bob_before) = (
        lamports(&svm, &alice.pubkey()),
        lamports(&svm, &bob.pubkey()),
    );
    let amount = a.min_bid * 2;
    let res = bid(&mut svm, &g, &bob, E8, 0, amount);
    assert_ok(&res);
    assert_eq!(
        lamports(&svm, &alice.pubkey()) - alice_before,
        a.min_bid,
        "alice refunded in full"
    );
    assert_eq!(
        bob_before - lamports(&svm, &bob.pubkey()),
        amount + base_fee(1)
    );
    assert_eq!(
        lamports(&svm, &addr),
        rent + amount,
        "escrow = rent + new bid"
    );
    let au = auction_of(&svm, &g.program, E8, 0);
    assert_eq!(
        (
            au.highest_bidder,
            au.highest_bid,
            au.bid_count,
            au.rent_payer
        ),
        (bob.pubkey(), amount, 2, alice.pubkey())
    );
    let ev: Vec<BidPlaced> = events(&res);
    assert_eq!(
        (
            ev[0].bidder,
            ev[0].amount,
            ev[0].refunded_to,
            ev[0].refunded
        ),
        (bob.pubkey(), amount, Some(alice.pubkey()), a.min_bid)
    );
}

#[test]
fn increment_boundary() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let (alice, bob) = (funded(&mut svm), funded(&mut svm));
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
    let next = ad_pool::math::min_next_bid(a.min_bid, 500).unwrap();
    assert_eq!(next, a.min_bid + (a.min_bid * 500).div_ceil(10_000));
    assert_custom_err(
        &bid(&mut svm, &g, &bob, E8, 0, next - 1),
        code(E::BidTooLow),
    );
    assert_custom_err(
        &bid(&mut svm, &g, &bob, E8, 0, a.min_bid),
        code(E::BidTooLow),
    );
    assert_ok(&bid(&mut svm, &g, &bob, E8, 0, next));
}

#[test]
fn self_raise_moves_only_the_difference() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let alice = funded(&mut svm);
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
    let addr = ad_slot_pda(&g.program, E8, 0);
    let rent = rent_min(&svm, AD_SLOT_AUCTION_SIZE as usize);
    let before = lamports(&svm, &alice.pubkey());
    let amount = a.min_bid * 2;
    let res = bid(&mut svm, &g, &alice, E8, 0, amount); // prev_bidder == alice
    assert_ok(&res);
    assert_eq!(
        before - lamports(&svm, &alice.pubkey()),
        amount - a.min_bid + base_fee(1)
    );
    assert_eq!(lamports(&svm, &addr), rent + amount);
    let ev: Vec<BidPlaced> = events(&res);
    assert_eq!((ev[0].refunded_to, ev[0].refunded), (None, 0));
    assert_eq!(auction_of(&svm, &g.program, E8, 0).bid_count, 2);
}

#[test]
fn stale_leader_fails_with_leader_changed() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let (alice, bob, carol) = (funded(&mut svm), funded(&mut svm), funded(&mut svm));
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
    // carol builds against alice, bob lands first
    let auction = ad_slot_pda(&g.program, E8, 0);
    let entry = game_entry_pda(&g.program).0;
    let carol_ix = ix_bid_with(
        &carol.pubkey(),
        &auction,
        &entry,
        &alice.pubkey(),
        a.min_bid * 3,
    );
    assert_ok(&bid(&mut svm, &g, &bob, E8, 0, a.min_bid * 2));
    assert_custom_err(
        &send(&mut svm, &[carol_ix], &[&carol]),
        code(E::LeaderChanged),
    );
    // a stale self-raise (alice thinks she still leads) fails the same way
    let alice_ix = ix_bid_with(
        &alice.pubkey(),
        &auction,
        &entry,
        &alice.pubkey(),
        a.min_bid * 3,
    );
    assert_custom_err(
        &send(&mut svm, &[alice_ix], &[&alice]),
        code(E::LeaderChanged),
    );
}

#[test]
fn outbid_succeeds_when_previous_leader_wallet_was_drained() {
    // design §5.3: refund >= floor >= rent(0), so crediting a deleted (0-lamport)
    // wallet can never fail → nobody can lock a slot by emptying their wallet.
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let (alice, bob) = (funded(&mut svm), funded(&mut svm));
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
    svm.set_account(
        alice.pubkey(),
        Account {
            lamports: 0,
            data: vec![],
            owner: anchor_lang::solana_program::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap();
    assert_eq!(lamports(&svm, &alice.pubkey()), 0);
    assert_ok(&bid(&mut svm, &g, &bob, E8, 0, a.min_bid * 2));
    assert_eq!(lamports(&svm, &alice.pubkey()), a.min_bid);
    assert!(a.min_bid >= rent_min(&svm, 0));
}

#[test]
fn bid_time_window_uses_snapshot() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let (alice, bob) = (funded(&mut svm), funded(&mut svm));
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
    // owner lengthens the lead afterwards — the open auction keeps its close_ts
    assert_ok(&send(
        &mut svm,
        &[ix_update_ad_config(
            &g.owner.pubkey(),
            &g.program,
            Some(20 * DAY),
            None,
            Some(10_000),
            None,
        )],
        &[&g.owner],
    ));
    let au = auction_of(&svm, &g.program, E8, 0);
    assert_eq!(
        (au.close_ts, au.min_increment_bps),
        (start_of(E8) - 7 * DAY, 500),
        "snapshot unchanged"
    );
    set_time(&mut svm, au.close_ts - 1);
    // increment is still the snapshot (5%), not the new 100%
    let next = ad_pool::math::min_next_bid(a.min_bid, 500).unwrap();
    assert_ok(&bid(&mut svm, &g, &bob, E8, 0, next));
    set_time(&mut svm, au.close_ts);
    let next2 = ad_pool::math::min_next_bid(next, 500).unwrap();
    assert_custom_err(
        &bid(&mut svm, &g, &alice, E8, 0, next2),
        code(E::BiddingClosed),
    );
}

#[test]
fn bid_rejects_game_entry_of_another_game_and_fake_auction() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let (g2, _) = new_configured_game(&mut svm);
    let (alice, bob) = (funded(&mut svm), funded(&mut svm));
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
    let auction = ad_slot_pda(&g.program, E8, 0);
    // GameEntry of another (Active) game — would bypass a Suspended check
    let ix = ix_bid_with(
        &bob.pubkey(),
        &auction,
        &game_entry_pda(&g2.program).0,
        &alice.pubkey(),
        a.min_bid * 2,
    );
    assert_custom_err(&send(&mut svm, &[ix], &[&bob]), 2006);
    // look-alike auction owned by another program
    let mut acct = svm.get_account(&auction).unwrap();
    acct.owner = Pubkey::new_unique();
    let fake = Pubkey::new_unique();
    svm.set_account(fake, acct).unwrap();
    let ix = ix_bid_with(
        &bob.pubkey(),
        &fake,
        &game_entry_pda(&g.program).0,
        &alice.pubkey(),
        a.min_bid * 2,
    );
    assert_custom_err(&send(&mut svm, &[ix], &[&bob]), 3007);
}

#[test]
fn bid_floor_check_runs_on_every_bid() {
    // floor is re-checked on every bid (design §5.3); with min_bid = floor and +5%
    // the increment rule already exceeds the floor, so this only guards the ordering.
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let (alice, bob) = (funded(&mut svm), funded(&mut svm));
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
    let f = floor(&svm);
    assert_custom_err(
        &bid(&mut svm, &g, &bob, E8, 0, f - 1),
        code(E::BidBelowFloor),
    );
}

#[test]
fn many_bids_keep_escrow_exact() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let bidders: Vec<Keypair> = (0..5).map(|_| funded(&mut svm)).collect();
    assert_ok(&open(&mut svm, &g, &bidders[0], E8, 0, a.min_bid));
    let mut amount = a.min_bid;
    // bidders 1..4 outbid in turn, then bidder 0 comes back
    for b in bidders[1..].iter().chain(bidders[..1].iter()) {
        amount = ad_pool::math::min_next_bid(amount, 500).unwrap();
        assert_ok(&bid(&mut svm, &g, b, E8, 0, amount));
    }
    let rent = rent_min(&svm, AD_SLOT_AUCTION_SIZE as usize);
    assert_eq!(
        lamports(&svm, &ad_slot_pda(&g.program, E8, 0)),
        rent + amount
    );
    let au = auction_of(&svm, &g.program, E8, 0);
    assert_eq!((au.highest_bidder, au.bid_count), (bidders[0].pubkey(), 6));
}

#[test]
fn donation_above_rent_stays_in_escrow_and_is_not_part_of_the_bid() {
    // Escrow invariant is `lamports >= rent + highest_bid` (donations add on top).
    // settle must pay out from `highest_bid` only (P4); close sweeps the rest to rent_payer.
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let alice = funded(&mut svm);
    let addr = ad_slot_pda(&g.program, E8, 0);
    let donation = 50_000_000;
    svm.set_account(
        addr,
        Account {
            lamports: donation,
            data: vec![],
            owner: anchor_lang::solana_program::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap();
    let before = lamports(&svm, &alice.pubkey());
    assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
    assert_eq!(
        before - lamports(&svm, &alice.pubkey()),
        a.min_bid + base_fee(1),
        "rent fully covered by donation"
    );
    assert_eq!(lamports(&svm, &addr), donation + a.min_bid);
    assert_eq!(auction_of(&svm, &g.program, E8, 0).highest_bid, a.min_bid);
}

#[test]
fn open_rejects_platform_config_at_wrong_address() {
    // A real PlatformConfig (owner = platform-core, right discriminator) at another
    // address must not be accepted: seeds [b"config"] + seeds::program pin it.
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let alice = funded(&mut svm);
    let real = svm.get_account(&pc_config_pda().0).unwrap();
    let fake = Pubkey::new_unique();
    svm.set_account(fake, real).unwrap();
    let mut ix = ix_open_auction(&alice.pubkey(), &g.program, E8, 0, a.min_bid);
    ix.accounts[2].pubkey = fake; // OpenAuction.config
    assert_custom_err(&send(&mut svm, &[ix], &[&alice]), 2006);
}

#[test]
fn open_rejects_ad_config_of_another_game() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let (g2, _) = new_configured_game(&mut svm);
    let alice = funded(&mut svm);
    let mut ix = ix_open_auction(&alice.pubkey(), &g.program, E8, 0, a.min_bid);
    ix.accounts[3].pubkey = ad_config_pda(&g2.program); // OpenAuction.ad_config
    assert_custom_err(&send(&mut svm, &[ix], &[&alice]), 2006);
}

#[test]
fn leader_turned_into_program_or_foreign_account_cannot_freeze_the_slot() {
    // design §5.3: crediting lamports has no owner/executable check in the runtime
    // (agave 4.3 under litesvm 0.17) — the outbid must still succeed.
    let so = program_so("ad_pool");
    let loader_v2: Pubkey = "BPFLoader2111111111111111111111111111111111"
        .parse()
        .unwrap();
    for (owner, executable, data) in [
        (loader_v2, true, so.clone()),
        (Pubkey::new_unique(), false, vec![1u8; 100]),
    ] {
        let mut svm = setup();
        let (g, a) = new_configured_game(&mut svm);
        let (alice, bob) = (funded(&mut svm), funded(&mut svm));
        assert_ok(&open(&mut svm, &g, &alice, E8, 0, a.min_bid));
        let lamports_now = rent_min(&svm, data.len());
        svm.set_account(
            alice.pubkey(),
            Account {
                lamports: lamports_now,
                data,
                owner,
                executable,
                rent_epoch: 0,
            },
        )
        .unwrap();
        assert_ok(&bid(&mut svm, &g, &bob, E8, 0, a.min_bid * 2));
        assert_eq!(lamports(&svm, &alice.pubkey()), lamports_now + a.min_bid);
    }
}
