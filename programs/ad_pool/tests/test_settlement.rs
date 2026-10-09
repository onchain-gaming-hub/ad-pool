//! P4 — settle + close_auction + full lifecycle (design §6 · §14 "settle / close").
#![allow(clippy::result_large_err)]

mod common;

use {
    ad_pool::{
        constants::*,
        error::AdPoolError as E,
        events::{AuctionClosed, Settled},
        platform_core::types::GameStatus,
        state::CreativeStatus,
    },
    anchor_lang::{prelude::Pubkey, solana_program::system_program},
    common::*,
    litesvm::LiteSVM,
    solana_account::Account,
    solana_keypair::Keypair,
    solana_signer::Signer,
};

const E8: u64 = FIRST_OPEN_EPOCH;

struct World {
    svm: LiteSVM,
    game: Game,
    alice: Keypair,
    auction: Pubkey,
    bid: u64,
    start: i64,
    end: i64,
}

/// Configured game + auction (E8, slot 0) opened by alice with `bid`.
fn world_with_bid(bid: u64) -> World {
    let mut svm = setup();
    let (game, _) = new_configured_game(&mut svm);
    let alice = funded(&mut svm);
    svm.airdrop(&alice.pubkey(), bid).unwrap();
    assert_ok(&send(
        &mut svm,
        &[ix_open_auction(&alice.pubkey(), &game.program, E8, 0, bid)],
        &[&alice],
    ));
    let au = auction_of(&svm, &game.program, E8, 0);
    World {
        auction: ad_slot_pda(&game.program, E8, 0),
        start: au.epoch_start_ts,
        end: au.epoch_end_ts,
        svm,
        game,
        alice,
        bid,
    }
}

fn world() -> World {
    let svm = setup();
    let f = floor(&svm);
    world_with_bid(f)
}

impl World {
    fn settle(&mut self) -> litesvm::types::TransactionResult {
        let cranker = funded(&mut self.svm);
        let ix = ix_settle(&self.svm, &self.game.program, E8, 0);
        send(&mut self.svm, &[ix], &[&cranker])
    }
    fn close(&mut self) -> litesvm::types::TransactionResult {
        let cranker = funded(&mut self.svm);
        let ix = ix_close_auction(&self.auction, &self.alice.pubkey());
        send(&mut self.svm, &[ix], &[&cranker])
    }
}

// ------------------------------------------------------------------- settle

#[test]
fn settle_pays_99_1_and_keeps_rent_in_escrow() {
    let mut w = world_with_bid(SOL);
    let rent = rent_min(&w.svm, AD_SLOT_AUCTION_SIZE as usize);
    let (owner, treasury) = (w.game.owner.pubkey(), treasury_pda().0);
    let (o0, t0) = (lamports(&w.svm, &owner), lamports(&w.svm, &treasury));
    set_time(&mut w.svm, w.start);
    let res = w.settle();
    assert_ok(&res);
    assert_eq!(lamports(&w.svm, &treasury) - t0, SOL / 100, "1% fee");
    assert_eq!(
        lamports(&w.svm, &owner) - o0,
        SOL * 99 / 100,
        "99% to GameEntry.owner"
    );
    assert_eq!(
        lamports(&w.svm, &w.auction),
        rent,
        "escrow keeps exactly rent"
    );
    let au = auction_of(&w.svm, &w.game.program, E8, 0);
    assert!(au.settled);
    assert_eq!(
        (au.highest_bid, au.updated_at),
        (SOL, w.start),
        "highest_bid kept as history"
    );
    let ev: Vec<Settled> = events(&res);
    assert_eq!(ev.len(), 1);
    assert_eq!(
        (ev[0].game_program, ev[0].epoch, ev[0].slot_index),
        (w.game.program, E8, 0)
    );
    assert_eq!(
        (ev[0].fee, ev[0].net, ev[0].game_owner),
        (SOL / 100, SOL * 99 / 100, owner)
    );
    assert_eq!((ev[0].winner, ev[0].amount), (w.alice.pubkey(), SOL));
}

#[test]
fn settle_timing_and_once_only() {
    let mut w = world();
    set_time(&mut w.svm, w.start - 1);
    assert_custom_err(&w.settle(), code(E::EpochNotStarted));
    set_time(&mut w.svm, w.start);
    assert_ok(&w.settle());
    assert_custom_err(&w.settle(), code(E::AlreadySettled));
    set_time(&mut w.svm, w.end + 10 * DAY);
    assert_custom_err(&w.settle(), code(E::AlreadySettled));
}

#[test]
fn settle_does_not_refund_even_without_creative() {
    // D4: winner pays in full even if nothing was ever approved.
    let mut w = world_with_bid(SOL);
    assert_eq!(
        auction_of(&w.svm, &w.game.program, E8, 0).creative_status,
        CreativeStatus::Empty
    );
    let a0 = lamports(&w.svm, &w.alice.pubkey());
    set_time(&mut w.svm, w.end);
    assert_ok(&w.settle());
    assert_eq!(
        lamports(&w.svm, &w.alice.pubkey()),
        a0,
        "no refund to the winner"
    );
}

#[test]
fn settle_works_when_game_suspended() {
    let mut w = world();
    put_game_entry(
        &mut w.svm,
        &w.game.program,
        &w.game.owner.pubkey(),
        GameStatus::Suspended,
    );
    set_time(&mut w.svm, w.start);
    assert_ok(&w.settle());
}

#[test]
fn settle_pays_the_owner_at_settle_time() {
    let mut w = world_with_bid(SOL);
    let new_owner = Pubkey::new_unique();
    put_game_entry(&mut w.svm, &w.game.program, &new_owner, GameStatus::Active);
    set_time(&mut w.svm, w.start);
    let old0 = lamports(&w.svm, &w.game.owner.pubkey());
    assert_ok(&w.settle());
    assert_eq!(lamports(&w.svm, &new_owner), SOL * 99 / 100);
    assert_eq!(lamports(&w.svm, &w.game.owner.pubkey()), old0);
}

#[test]
fn settle_pays_non_system_owner_and_empty_wallet() {
    // Q12: UncheckedAccount — a PDA / multisig owner must not block settle (or rent).
    for (owner_acct, label) in [
        (
            Account {
                lamports: 1_000_000,
                data: vec![7; 64],
                owner: Pubkey::new_unique(),
                executable: false,
                rent_epoch: 0,
            },
            "program-owned",
        ),
        (
            Account {
                lamports: 0,
                data: vec![],
                owner: system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
            "0-lamport wallet",
        ),
    ] {
        let mut w = world();
        let owner = Pubkey::new_unique();
        let before = owner_acct.lamports;
        if before > 0 {
            w.svm.set_account(owner, owner_acct).unwrap();
        }
        put_game_entry(&mut w.svm, &w.game.program, &owner, GameStatus::Active);
        set_time(&mut w.svm, w.start);
        assert_ok(&w.settle());
        let (_, net) = ad_pool::math::fee_split(w.bid, 100).unwrap();
        assert_eq!(lamports(&w.svm, &owner), before + net, "{label}");
    }
}

#[test]
fn settle_pays_from_highest_bid_only_donations_stay() {
    let mut w = world_with_bid(SOL);
    let rent = rent_min(&w.svm, AD_SLOT_AUCTION_SIZE as usize);
    let donation = 12_345_678;
    let mut acct = w.svm.get_account(&w.auction).unwrap();
    acct.lamports += donation;
    w.svm.set_account(w.auction, acct).unwrap();
    let o0 = lamports(&w.svm, &w.game.owner.pubkey());
    set_time(&mut w.svm, w.start);
    assert_ok(&w.settle());
    assert_eq!(
        lamports(&w.svm, &w.game.owner.pubkey()) - o0,
        SOL * 99 / 100
    );
    assert_eq!(
        lamports(&w.svm, &w.auction),
        rent + donation,
        "escrow after settle == rent + donation"
    );
    // close sweeps everything to rent_payer
    set_time(&mut w.svm, w.end);
    let a0 = lamports(&w.svm, &w.alice.pubkey());
    assert_ok(&w.close());
    assert_eq!(lamports(&w.svm, &w.alice.pubkey()) - a0, rent + donation);
}

#[test]
fn settle_uses_the_fee_snapshot() {
    for bps in [0u16, 100, MAX_FEE_BPS] {
        let mut svm = setup();
        put_pc_config(&mut svm, bps);
        let (game, _) = new_configured_game(&mut svm);
        let alice = funded(&mut svm);
        assert_ok(&send(
            &mut svm,
            &[ix_open_auction(&alice.pubkey(), &game.program, E8, 0, SOL)],
            &[&alice],
        ));
        put_pc_config(&mut svm, 1_234); // later change in platform-core is ignored
        let t0 = lamports(&svm, &treasury_pda().0);
        let start = auction_of(&svm, &game.program, E8, 0).epoch_start_ts;
        set_time(&mut svm, start - 1);
        let cranker0 = funded(&mut svm);
        let early = ix_settle(&svm, &game.program, E8, 0);
        assert_custom_err(
            &send(&mut svm, &[early], &[&cranker0]),
            code(E::EpochNotStarted),
        );
        set_time(&mut svm, start);
        let cranker = funded(&mut svm);
        let ix = ix_settle(&svm, &game.program, E8, 0);
        assert_ok(&send(&mut svm, &[ix], &[&cranker]));
        assert_eq!(
            lamports(&svm, &treasury_pda().0) - t0,
            SOL * bps as u64 / 10_000,
            "bps {bps}"
        );
    }
}

#[test]
fn settle_rejects_substituted_accounts() {
    let mut w = world();
    let (g2, _) = new_configured_game(&mut w.svm);
    set_time(&mut w.svm, w.start);
    let cranker = funded(&mut w.svm);
    let entry = game_entry_pda(&w.game.program).0;
    let owner = w.game.owner.pubkey();
    let t = treasury_pda().0;
    // GameEntry (and so payee) of another game
    let ix = ix_settle_with(
        &w.auction,
        &game_entry_pda(&g2.program).0,
        &g2.owner.pubkey(),
        &t,
    );
    assert_custom_err(&send(&mut w.svm, &[ix], &[&cranker]), 2006);
    // payee is not GameEntry.owner
    let ix = ix_settle_with(&w.auction, &entry, &cranker.pubkey(), &t);
    assert_custom_err(&send(&mut w.svm, &[ix], &[&cranker]), code(E::NotGameOwner));
    // fee to an attacker instead of the Treasury PDA
    let ix = ix_settle_with(&w.auction, &entry, &owner, &cranker.pubkey());
    assert_custom_err(&send(&mut w.svm, &[ix], &[&cranker]), 2006);
    assert!(!auction_of(&w.svm, &w.game.program, E8, 0).settled);
}

// -------------------------------------------------------------------- close

#[test]
fn close_returns_everything_to_rent_payer() {
    let mut w = world();
    let rent = rent_min(&w.svm, AD_SLOT_AUCTION_SIZE as usize);
    set_time(&mut w.svm, w.start);
    assert_ok(&w.settle());
    set_time(&mut w.svm, w.end);
    let a0 = lamports(&w.svm, &w.alice.pubkey());
    let res = w.close(); // a third party pays the fee
    assert_ok(&res);
    assert_eq!(lamports(&w.svm, &w.alice.pubkey()) - a0, rent);
    let gone = w.svm.get_account(&w.auction);
    assert!(
        gone.is_none() || gone.unwrap().lamports == 0,
        "account closed"
    );
    let ev: Vec<AuctionClosed> = events(&res);
    assert_eq!(
        (ev[0].rent_payer, ev[0].refunded, ev[0].epoch),
        (w.alice.pubkey(), rent, E8)
    );
}

#[test]
fn close_conditions() {
    let mut w = world();
    set_time(&mut w.svm, w.end);
    assert_custom_err(&w.close(), code(E::NotSettled));
    set_time(&mut w.svm, w.start);
    assert_ok(&w.settle());
    set_time(&mut w.svm, w.end - 1);
    assert_custom_err(&w.close(), code(E::EpochNotEnded));
    set_time(&mut w.svm, w.end);
    let cranker = funded(&mut w.svm);
    let ix = ix_close_auction(&w.auction, &cranker.pubkey());
    assert_custom_err(
        &send(&mut w.svm, &[ix], &[&cranker]),
        code(E::RentPayerMismatch),
    );
    assert_ok(&w.close());
}

#[test]
fn close_works_after_owner_became_non_system() {
    let mut w = world();
    set_time(&mut w.svm, w.start);
    assert_ok(&w.settle());
    put_game_entry(
        &mut w.svm,
        &w.game.program,
        &Pubkey::new_unique(),
        GameStatus::Suspended,
    );
    set_time(&mut w.svm, w.end);
    assert_ok(&w.close());
}

#[test]
fn closed_auction_cannot_be_reopened_or_moderated() {
    let mut w = world();
    let moderator = new_moderator(&mut w.svm);
    set_time(&mut w.svm, w.start);
    assert_ok(&w.settle());
    set_time(&mut w.svm, w.end);
    assert_ok(&w.close());
    let bob = funded(&mut w.svm);
    let f = floor(&w.svm);
    let res = send(
        &mut w.svm,
        &[ix_open_auction(&bob.pubkey(), &w.game.program, E8, 0, f)],
        &[&bob],
    );
    assert_custom_err(&res, code(E::BiddingClosed));
    let ix = ix_reject_creative(&moderator.pubkey(), &w.auction, 1);
    assert_custom_err(&send(&mut w.svm, &[ix], &[&moderator]), 3012);
}

// ---------------------------------------------------------------- lifecycle

#[test]
fn full_lifecycle_lamports_add_up() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let (alice, bob, carol) = (funded(&mut svm), funded(&mut svm), funded(&mut svm));
    let moderator = new_moderator(&mut svm);
    let (owner, treasury) = (g.owner.pubkey(), treasury_pda().0);
    let start_bal = |svm: &LiteSVM| {
        [
            alice.pubkey(),
            bob.pubkey(),
            carol.pubkey(),
            owner,
            treasury,
        ]
        .map(|k| lamports(svm, &k))
    };
    let b0 = start_bal(&svm);

    // three bids
    assert_ok(&send(
        &mut svm,
        &[ix_open_auction(
            &alice.pubkey(),
            &g.program,
            E8,
            0,
            a.min_bid,
        )],
        &[&alice],
    ));
    let b2 = ad_pool::math::min_next_bid(a.min_bid, 500).unwrap() * 3;
    let ix = ix_bid(&svm, &bob.pubkey(), &g.program, E8, 0, b2);
    assert_ok(&send(&mut svm, &[ix], &[&bob]));
    let win = ad_pool::math::min_next_bid(b2, 500).unwrap();
    let ix = ix_bid(&svm, &carol.pubkey(), &g.program, E8, 0, win);
    assert_ok(&send(&mut svm, &[ix], &[&carol]));
    let auction = ad_slot_pda(&g.program, E8, 0);
    let au = auction_of(&svm, &g.program, E8, 0);

    // creative: text-only (image failed) → image approved
    set_time(&mut svm, au.close_ts);
    let ix = ix_approve_creative(
        &moderator.pubkey(),
        &auction,
        text_only_creative(),
        REJECT_IMAGE_MODERATION,
    );
    assert_ok(&send(&mut svm, &[ix], &[&moderator]));
    let ix = ix_approve_image(
        &moderator.pubkey(),
        &auction,
        "https://ads.example.r2.dev/ads/ef.png".into(),
        [0xEF; 32],
    );
    assert_ok(&send(&mut svm, &[ix], &[&moderator]));
    assert_eq!(
        auction_of(&svm, &g.program, E8, 0).creative_status,
        CreativeStatus::Approved
    );

    // settle at epoch start, close at epoch end (cranks paid by the moderator key here)
    set_time(&mut svm, au.epoch_start_ts);
    let ix = ix_settle(&svm, &g.program, E8, 0);
    assert_ok(&send(&mut svm, &[ix], &[&moderator]));
    set_time(&mut svm, au.epoch_end_ts);
    assert_ok(&send(
        &mut svm,
        &[ix_close_auction(&auction, &alice.pubkey())],
        &[&moderator],
    ));

    let b1 = start_bal(&svm);
    let (fee, net) = ad_pool::math::fee_split(win, 100).unwrap();
    // alice: paid a.min_bid + rent + 1 fee, got a.min_bid back (outbid) + rent back (close)
    assert_eq!(b0[0] - b1[0], base_fee(1), "alice: only her tx fee");
    assert_eq!(b0[1] - b1[1], base_fee(1), "bob: refunded in full");
    assert_eq!(
        b0[2] - b1[2],
        win + base_fee(1),
        "carol: pays the winning bid"
    );
    assert_eq!(b1[3] - b0[3], net, "owner: 99%");
    assert_eq!(b1[4] - b0[4], fee, "treasury: 1%");
    assert_eq!(fee + net, win);
    let gone = svm.get_account(&auction);
    assert!(gone.is_none() || gone.unwrap().lamports == 0);
}

// ----------------------------------------------------------------------- CU

#[test]
fn compute_units_per_instruction() {
    let mut svm = setup();
    let g = new_game(&mut svm);
    let a = ConfigArgs::default_with_floor(floor(&svm));
    let r_init = send(
        &mut svm,
        &[ix_init_ad_config(&g.owner.pubkey(), &g.program, &a)],
        &[&g.owner],
    );
    let (alice, bob) = (funded(&mut svm), funded(&mut svm));
    let moderator = new_moderator(&mut svm);
    let r_open = send(
        &mut svm,
        &[ix_open_auction(
            &alice.pubkey(),
            &g.program,
            E8,
            0,
            a.min_bid,
        )],
        &[&alice],
    );
    let ix = ix_bid(&svm, &bob.pubkey(), &g.program, E8, 0, a.min_bid * 2);
    let r_bid = send(&mut svm, &[ix], &[&bob]);
    let au = auction_of(&svm, &g.program, E8, 0);
    let auction = ad_slot_pda(&g.program, E8, 0);
    set_time(&mut svm, au.close_ts);
    let ix = ix_approve_creative(&moderator.pubkey(), &auction, full_creative(), 0);
    let r_appr = send(&mut svm, &[ix], &[&moderator]);
    set_time(&mut svm, au.epoch_start_ts);
    let ix = ix_settle(&svm, &g.program, E8, 0);
    let r_settle = send(&mut svm, &[ix], &[&moderator]);
    set_time(&mut svm, au.epoch_end_ts);
    let r_close = send(
        &mut svm,
        &[ix_close_auction(&auction, &alice.pubkey())],
        &[&moderator],
    );
    let rows = [
        ("init_ad_config", &r_init),
        ("open_auction", &r_open),
        ("bid", &r_bid),
        ("approve_creative", &r_appr),
        ("settle", &r_settle),
        ("close_auction", &r_close),
    ];
    for (name, r) in rows {
        assert_ok(r);
        println!("CU {name}: {}", cu(r));
        assert!(cu(r) < 60_000, "{name} uses {} CU", cu(r));
    }
}

#[test]
fn settle_and_close_in_one_transaction() {
    // What the Worker crank batches after epoch end.
    let mut w = world_with_bid(SOL);
    let rent = rent_min(&w.svm, AD_SLOT_AUCTION_SIZE as usize);
    set_time(&mut w.svm, w.end);
    let cranker = funded(&mut w.svm);
    let (o0, a0) = (
        lamports(&w.svm, &w.game.owner.pubkey()),
        lamports(&w.svm, &w.alice.pubkey()),
    );
    let settle = ix_settle(&w.svm, &w.game.program, E8, 0);
    let close = ix_close_auction(&w.auction, &w.alice.pubkey());
    assert_ok(&send(&mut w.svm, &[settle, close], &[&cranker]));
    assert_eq!(
        lamports(&w.svm, &w.game.owner.pubkey()) - o0,
        SOL * 99 / 100
    );
    assert_eq!(lamports(&w.svm, &w.alice.pubkey()) - a0, rent);
    // a double settle in one tx is rejected as a whole
    let mut w = world_with_bid(SOL);
    set_time(&mut w.svm, w.start);
    let cranker = funded(&mut w.svm);
    let s1 = ix_settle(&w.svm, &w.game.program, E8, 0);
    let res = send(&mut w.svm, &[s1.clone(), s1], &[&cranker]);
    let err = format!("{:?}", res.as_ref().unwrap_err().err);
    assert!(
        err.contains(&format!(
            "InstructionError(1, Custom({}))",
            code(E::AlreadySettled)
        )),
        "{err}"
    );
    assert!(
        !auction_of(&w.svm, &w.game.program, E8, 0).settled,
        "rolled back"
    );
}

#[test]
fn settle_blocks_if_admin_sets_owner_to_a_reserved_key() {
    // Accepted residual (design §6.1 / Q12): a reserved/builtin key is demoted to
    // read-only → `mut` fails (ConstraintMut 2000) and the auction stays unsettled
    // until platform-core's Admin fixes GameEntry.owner. Only Admin can cause this.
    let mut w = world();
    let compute_budget: Pubkey = "ComputeBudget111111111111111111111111111111"
        .parse()
        .unwrap();
    put_game_entry(
        &mut w.svm,
        &w.game.program,
        &compute_budget,
        GameStatus::Active,
    );
    set_time(&mut w.svm, w.start);
    assert_custom_err(&w.settle(), 2000);
    assert!(!auction_of(&w.svm, &w.game.program, E8, 0).settled);
    // Admin repairs the owner → settle works again
    put_game_entry(
        &mut w.svm,
        &w.game.program,
        &w.game.owner.pubkey(),
        GameStatus::Active,
    );
    assert_ok(&w.settle());
}
