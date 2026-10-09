//! P1 — config instructions (design §8 · §4.2 · §5.3 · §14 "config").

// `TransactionResult` is litesvm's type; its large Err variant is not ours to box.
#![allow(clippy::result_large_err)]

mod common;

use {
    ad_pool::{
        constants::*,
        error::AdPoolError as E,
        events::{AdConfigInitialized, AdConfigUpdated, SlotActiveSet, SlotAdded},
        platform_core::types::GameStatus,
        state::GameAdConfig,
    },
    anchor_lang::prelude::Pubkey,
    common::*,
    solana_signer::Signer,
};

fn init_err(a: &ConfigArgs) -> litesvm::types::TransactionResult {
    let mut svm = setup();
    let g = new_game(&mut svm);
    send(
        &mut svm,
        &[ix_init_ad_config(&g.owner.pubkey(), &g.program, a)],
        &[&g.owner],
    )
}

fn base(svm: &litesvm::LiteSVM) -> ConfigArgs {
    ConfigArgs::default_with_floor(floor(svm))
}

// ------------------------------------------------------------------ init

#[test]
fn init_writes_every_field_and_emits_events() {
    let mut svm = setup();
    let g = new_game(&mut svm);
    let a = base(&svm);
    let owner_before = lamports(&svm, &g.owner.pubkey());
    let res = send(
        &mut svm,
        &[ix_init_ad_config(&g.owner.pubkey(), &g.program, &a)],
        &[&g.owner],
    );
    assert_ok(&res);

    let addr = ad_config_pda(&g.program);
    let cfg: GameAdConfig = fetch(&svm, &addr);
    assert_eq!(cfg.version, ACCOUNT_VERSION);
    assert_eq!(
        cfg.bump,
        Pubkey::find_program_address(&[AD_CONFIG_SEED, g.program.as_ref()], &ad_pool::id()).1
    );
    assert_eq!(cfg.game_program, g.program);
    assert_eq!(cfg.slot_count, 2);
    assert_eq!(
        (cfg.slots[0].width, cfg.slots[0].height, cfg.slots[0].active),
        (728, 90, true)
    );
    assert_eq!(
        (cfg.slots[1].width, cfg.slots[1].height, cfg.slots[1].active),
        (300, 250, true)
    );
    assert!(cfg.slots[2..]
        .iter()
        .all(|s| s.width == 0 && s.height == 0 && !s.active));
    assert_eq!(cfg.genesis_ts, a.genesis_ts);
    assert_eq!(cfg.epoch_duration_secs, DAY);
    assert_eq!(cfg.auction_close_lead_secs, 7 * DAY);
    assert_eq!(cfg.min_bid, a.min_bid);
    assert_eq!(cfg.min_increment_bps, 500);
    assert_eq!(cfg.bid_horizon_epochs, 30);
    assert_eq!((cfg.created_at, cfg.updated_at), (T0, T0));
    assert_eq!(cfg._reserved, [0; 64]);

    // rent paid by the owner, account sized per layout
    let acct = svm.get_account(&addr).unwrap();
    assert_eq!(acct.data.len(), GAME_AD_CONFIG_SIZE as usize);
    assert_eq!(acct.owner, ad_pool::id());
    assert_eq!(acct.lamports, rent_min(&svm, GAME_AD_CONFIG_SIZE as usize));

    // the game owner paid exactly rent + tx fee (D11)
    assert_eq!(
        owner_before - lamports(&svm, &g.owner.pubkey()),
        acct.lamports + base_fee(1)
    );

    // 1 AdConfigInitialized + 1 SlotAdded per initial slot (decision P1)
    let init: Vec<AdConfigInitialized> = events(&res);
    assert_eq!(init.len(), 1);
    let e = &init[0];
    assert_eq!(
        (e.game_program, e.game_owner),
        (g.program, g.owner.pubkey())
    );
    assert_eq!(
        (
            e.genesis_ts,
            e.epoch_duration_secs,
            e.auction_close_lead_secs
        ),
        (a.genesis_ts, DAY, 7 * DAY)
    );
    assert_eq!(
        (
            e.min_bid,
            e.min_increment_bps,
            e.bid_horizon_epochs,
            e.slot_count
        ),
        (a.min_bid, 500, 30, 2)
    );
    let added: Vec<SlotAdded> = events(&res);
    let got: Vec<_> = added
        .iter()
        .map(|s| (s.game_program, s.slot_index, s.width, s.height))
        .collect();
    assert_eq!(got, vec![(g.program, 0, 728, 90), (g.program, 1, 300, 250)]);
}

#[test]
fn init_twice_fails() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let addr = ad_config_pda(&g.program);
    let before = svm.get_account(&addr).unwrap().data;
    let mut b = a.clone();
    b.min_bid = a.min_bid * 3;
    b.slots = vec![(1, 1)];
    let res = send(
        &mut svm,
        &[ix_init_ad_config(&g.owner.pubkey(), &g.program, &b)],
        &[&g.owner],
    );
    assert_custom_err(&res, 0); // System AccountAlreadyInUse
    assert_eq!(
        svm.get_account(&addr).unwrap().data,
        before,
        "not overwritten"
    );
}

#[test]
fn init_requires_game_owner() {
    let mut svm = setup();
    let g = new_game(&mut svm);
    let stranger = funded(&mut svm);
    let a = base(&svm);
    let res = send(
        &mut svm,
        &[ix_init_ad_config(&stranger.pubkey(), &g.program, &a)],
        &[&stranger],
    );
    assert_custom_err(&res, code(E::NotGameOwner));
}

#[test]
fn init_requires_active_game() {
    let mut svm = setup();
    let owner = funded(&mut svm);
    let program = Pubkey::new_unique();
    put_game_entry(&mut svm, &program, &owner.pubkey(), GameStatus::Suspended);
    let a = base(&svm);
    let res = send(
        &mut svm,
        &[ix_init_ad_config(&owner.pubkey(), &program, &a)],
        &[&owner],
    );
    assert_custom_err(&res, code(E::GameNotActive));
}

#[test]
fn init_rejects_unregistered_game() {
    let mut svm = setup();
    let owner = funded(&mut svm);
    let a = base(&svm);
    let res = send(
        &mut svm,
        &[ix_init_ad_config(
            &owner.pubkey(),
            &Pubkey::new_unique(),
            &a,
        )],
        &[&owner],
    );
    assert_custom_err(&res, 3012); // AccountNotInitialized (no GameEntry)
}

#[test]
fn init_rejects_game_entry_not_owned_by_platform_core() {
    let mut svm = setup();
    let owner = funded(&mut svm);
    let program = Pubkey::new_unique();
    put_fake_game_entry(&mut svm, &program, &owner.pubkey(), Pubkey::new_unique());
    let a = base(&svm);
    let res = send(
        &mut svm,
        &[ix_init_ad_config(&owner.pubkey(), &program, &a)],
        &[&owner],
    );
    assert_custom_err(&res, 3007); // AccountOwnedByWrongProgram
}

#[test]
fn init_rejects_game_entry_of_another_game() {
    // Owner of game A cannot create a config for game B by passing A's GameEntry.
    let mut svm = setup();
    let a_game = new_game(&mut svm);
    let b_program = Pubkey::new_unique();
    put_game_entry(
        &mut svm,
        &b_program,
        &Pubkey::new_unique(),
        GameStatus::Active,
    );
    let a = base(&svm);
    let ix = ix_init_ad_config_with(
        &a_game.owner.pubkey(),
        &b_program,
        &game_entry_pda(&a_game.program).0,
        &a,
    );
    let res = send(&mut svm, &[ix], &[&a_game.owner]);
    assert_custom_err(&res, 2006); // ConstraintSeeds
}

#[test]
fn init_validates_epoch_duration() {
    let svm = setup();
    for d in [0, -DAY, DAY + 1, 31 * DAY] {
        let mut a = base(&svm);
        a.epoch_duration_secs = d;
        assert_custom_err(&init_err(&a), code(E::InvalidEpochDuration));
    }
    // 30 days is the cap (Q11) — valid with horizon 3 × 30 d = 90 d
    let mut a = base(&svm);
    a.epoch_duration_secs = 30 * DAY;
    a.bid_horizon_epochs = 3;
    assert_ok(&init_err(&a));
}

#[test]
fn init_validates_genesis_is_utc_midnight_and_non_negative() {
    let svm = setup();
    for gts in [T0 + 1, T0 + 7 * 3_600, -DAY] {
        let mut a = base(&svm);
        a.genesis_ts = gts;
        assert_custom_err(&init_err(&a), code(E::InvalidGenesis));
    }
    // genesis in the past or future is fine as long as it is a UTC day start
    for gts in [0, T0 - 100 * DAY, T0 + 10 * DAY] {
        let mut a = base(&svm);
        a.genesis_ts = gts;
        assert_ok(&init_err(&a));
    }
}

#[test]
fn init_validates_close_lead_at_least_7_days() {
    let svm = setup();
    let mut a = base(&svm);
    a.auction_close_lead_secs = 7 * DAY - 1;
    assert_custom_err(&init_err(&a), code(E::CloseLeadTooShort));
    a.auction_close_lead_secs = 7 * DAY;
    assert_ok(&init_err(&a));
}

#[test]
fn init_validates_increment() {
    let svm = setup();
    for bps in [0u16, 10_001] {
        let mut a = base(&svm);
        a.min_increment_bps = bps;
        assert_custom_err(&init_err(&a), code(E::InvalidIncrement));
    }
    for bps in [1u16, 10_000] {
        let mut a = base(&svm);
        a.min_increment_bps = bps;
        assert_ok(&init_err(&a));
    }
}

#[test]
fn init_validates_horizon_bounds() {
    let svm = setup();
    // lead 7 d + 1 epoch → horizon must be >= 8 epochs of 1 day
    let mut a = base(&svm);
    a.bid_horizon_epochs = 7;
    assert_custom_err(&init_err(&a), code(E::HorizonTooShort));
    a.bid_horizon_epochs = 8;
    assert_ok(&init_err(&a));
    // horizon × duration <= 90 days
    a.bid_horizon_epochs = 90;
    assert_ok(&init_err(&a));
    a.bid_horizon_epochs = 91;
    assert_custom_err(&init_err(&a), code(E::HorizonTooLong));
    a.bid_horizon_epochs = u16::MAX;
    assert_custom_err(&init_err(&a), code(E::HorizonTooLong));
}

#[test]
fn init_validates_min_bid_floor() {
    let svm = setup();
    let f = floor(&svm);
    assert_eq!(
        f,
        rent_min(&svm, AD_SLOT_AUCTION_SIZE as usize),
        "Q6: floor = rent(779)"
    );
    let mut a = base(&svm);
    a.min_bid = f - 1;
    assert_custom_err(&init_err(&a), code(E::MinBidBelowFloor));
    a.min_bid = f;
    assert_ok(&init_err(&a));
}

#[test]
fn init_validates_slots() {
    let svm = setup();
    let cases: Vec<(Vec<(u16, u16)>, u32)> = vec![
        (vec![], code(E::InvalidSlot)),
        (vec![(0, 90)], code(E::InvalidSlot)),
        (vec![(728, 0)], code(E::InvalidSlot)),
        (vec![(1, 1); 9], code(E::TooManySlots)),
    ];
    for (slots, err) in cases {
        let mut a = base(&svm);
        a.slots = slots;
        assert_custom_err(&init_err(&a), err);
    }
    let mut a = base(&svm);
    a.slots = vec![(1, 1); 8];
    assert_ok(&init_err(&a));
}

// ---------------------------------------------------------------- update

#[test]
fn update_changes_only_given_fields_and_keeps_immutables() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    set_time(&mut svm, T0 + 3_600);
    let new_min = a.min_bid * 2;
    assert_ok(&send(
        &mut svm,
        &[ix_update_ad_config(
            &g.owner.pubkey(),
            &g.program,
            None,
            Some(new_min),
            None,
            Some(20),
        )],
        &[&g.owner],
    ));
    let cfg: GameAdConfig = fetch(&svm, &ad_config_pda(&g.program));
    assert_eq!(cfg.min_bid, new_min);
    assert_eq!(cfg.bid_horizon_epochs, 20);
    assert_eq!(cfg.auction_close_lead_secs, a.auction_close_lead_secs);
    assert_eq!(cfg.min_increment_bps, a.min_increment_bps);
    assert_eq!(cfg.genesis_ts, a.genesis_ts);
    assert_eq!(cfg.epoch_duration_secs, a.epoch_duration_secs);
    assert_eq!((cfg.created_at, cfg.updated_at), (T0, T0 + 3_600));
}

#[test]
fn update_revalidates_the_combined_config() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let owner = g.owner.pubkey();
    // lead 20 d needs horizon >= 21 epochs; current horizon is 30 → OK
    assert_ok(&send(
        &mut svm,
        &[ix_update_ad_config(
            &owner,
            &g.program,
            Some(20 * DAY),
            None,
            None,
            None,
        )],
        &[&g.owner],
    ));
    // now shrinking horizon to 20 conflicts with lead 20 d
    let res = send(
        &mut svm,
        &[ix_update_ad_config(
            &owner,
            &g.program,
            None,
            None,
            None,
            Some(20),
        )],
        &[&g.owner],
    );
    assert_custom_err(&res, code(E::HorizonTooShort));
    let res = send(
        &mut svm,
        &[ix_update_ad_config(
            &owner,
            &g.program,
            Some(DAY),
            None,
            None,
            None,
        )],
        &[&g.owner],
    );
    assert_custom_err(&res, code(E::CloseLeadTooShort));
    let res = send(
        &mut svm,
        &[ix_update_ad_config(
            &owner,
            &g.program,
            None,
            Some(a.min_bid - 1),
            None,
            None,
        )],
        &[&g.owner],
    );
    assert_custom_err(&res, code(E::MinBidBelowFloor));
    let res = send(
        &mut svm,
        &[ix_update_ad_config(
            &owner,
            &g.program,
            None,
            None,
            Some(0),
            None,
        )],
        &[&g.owner],
    );
    assert_custom_err(&res, code(E::InvalidIncrement));
}

#[test]
fn update_requires_game_owner() {
    let mut svm = setup();
    let (g, _) = new_configured_game(&mut svm);
    let stranger = funded(&mut svm);
    let res = send(
        &mut svm,
        &[ix_update_ad_config(
            &stranger.pubkey(),
            &g.program,
            None,
            None,
            None,
            Some(10),
        )],
        &[&stranger],
    );
    assert_custom_err(&res, code(E::NotGameOwner));
}

#[test]
fn update_emits_full_resulting_values() {
    let mut svm = setup();
    let (g, a) = new_configured_game(&mut svm);
    let ix = ix_update_ad_config(
        &g.owner.pubkey(),
        &g.program,
        Some(8 * DAY),
        None,
        Some(250),
        None,
    );
    let res = send(&mut svm, &[ix], &[&g.owner]);
    assert_ok(&res);
    let ev: Vec<AdConfigUpdated> = events(&res);
    assert_eq!(ev.len(), 1);
    let e = &ev[0];
    assert_eq!(e.game_program, g.program);
    assert_eq!(
        (
            e.auction_close_lead_secs,
            e.min_bid,
            e.min_increment_bps,
            e.bid_horizon_epochs
        ),
        (8 * DAY, a.min_bid, 250, 30)
    );
}

#[test]
fn update_with_no_changes_is_allowed() {
    // Documented: a no-op update succeeds (bumps updated_at, emits current values).
    let mut svm = setup();
    let (g, _) = new_configured_game(&mut svm);
    let ix = ix_update_ad_config(&g.owner.pubkey(), &g.program, None, None, None, None);
    assert_ok(&send(&mut svm, &[ix], &[&g.owner]));
}

#[test]
fn manage_rejects_game_entry_of_another_game() {
    // Owner of game A passes A's own GameEntry with B's config → seeds derived from
    // B's stored game_program mismatch → ConstraintSeeds. update / add_slot /
    // set_slot_active share ManageAdConfig, so this covers all three.
    let mut svm = setup();
    let (a_game, _) = new_configured_game(&mut svm);
    let (b_game, _) = new_configured_game(&mut svm);
    let a_entry = game_entry_pda(&a_game.program).0;
    let ix = ix_add_slot_with_entry(&a_game.owner.pubkey(), &b_game.program, &a_entry);
    assert_custom_err(&send(&mut svm, &[ix], &[&a_game.owner]), 2006);
    let cfg: GameAdConfig = fetch(&svm, &ad_config_pda(&b_game.program));
    assert_eq!(cfg.slot_count, 2, "B unchanged");
}

#[test]
fn manage_rejects_game_entry_not_owned_by_platform_core() {
    let mut svm = setup();
    let (g, _) = new_configured_game(&mut svm);
    put_fake_game_entry(
        &mut svm,
        &g.program,
        &g.owner.pubkey(),
        Pubkey::new_unique(),
    );
    let entry = game_entry_pda(&g.program).0;
    let ix = ix_add_slot_with_entry(&g.owner.pubkey(), &g.program, &entry);
    assert_custom_err(&send(&mut svm, &[ix], &[&g.owner]), 3007);
}

#[test]
fn owner_follows_game_entry_owner_changes() {
    // Ownership lives in platform-core only (rev 3 #4): a transfer there moves the right here.
    let mut svm = setup();
    let (g, _) = new_configured_game(&mut svm);
    let new_owner = funded(&mut svm);
    put_game_entry(
        &mut svm,
        &g.program,
        &new_owner.pubkey(),
        GameStatus::Active,
    );
    let res = send(
        &mut svm,
        &[ix_add_slot(&g.owner.pubkey(), &g.program, 160, 600)],
        &[&g.owner],
    );
    assert_custom_err(&res, code(E::NotGameOwner));
    assert_ok(&send(
        &mut svm,
        &[ix_add_slot(&new_owner.pubkey(), &g.program, 160, 600)],
        &[&new_owner],
    ));
}

#[test]
fn owner_edits_still_work_when_game_suspended() {
    // pc 03 §1 rule 3: Active is checked only when starting something new.
    let mut svm = setup();
    let (g, _) = new_configured_game(&mut svm);
    put_game_entry(
        &mut svm,
        &g.program,
        &g.owner.pubkey(),
        GameStatus::Suspended,
    );
    let o = g.owner.pubkey();
    assert_ok(&send(
        &mut svm,
        &[ix_set_slot_active(&o, &g.program, 0, false)],
        &[&g.owner],
    ));
    assert_ok(&send(
        &mut svm,
        &[ix_add_slot(&o, &g.program, 160, 600)],
        &[&g.owner],
    ));
    assert_ok(&send(
        &mut svm,
        &[ix_update_ad_config(
            &o,
            &g.program,
            None,
            None,
            Some(1_000),
            None,
        )],
        &[&g.owner],
    ));
}

// ----------------------------------------------------------- slots

#[test]
fn add_slot_appends_up_to_eight() {
    let mut svm = setup();
    let (g, _) = new_configured_game(&mut svm);
    let o = g.owner.pubkey();
    for i in 2..8u16 {
        set_time(&mut svm, T0 + i as i64);
        let res = send(
            &mut svm,
            &[ix_add_slot(&o, &g.program, 100 + i, 50 + i)],
            &[&g.owner],
        );
        assert_ok(&res);
        let ev: Vec<SlotAdded> = events(&res);
        assert_eq!(
            (
                ev[0].game_program,
                ev[0].slot_index,
                ev[0].width,
                ev[0].height
            ),
            (g.program, i as u8, 100 + i, 50 + i)
        );
        let cfg: GameAdConfig = fetch(&svm, &ad_config_pda(&g.program));
        assert_eq!(cfg.updated_at, T0 + i as i64);
    }
    let cfg: GameAdConfig = fetch(&svm, &ad_config_pda(&g.program));
    assert_eq!(cfg.slot_count, 8);
    assert!(cfg.slots.iter().all(|s| s.active));
    assert_eq!(
        (cfg.slots[7].width, cfg.slots[7].height, cfg.slots[7].active),
        (107, 57, true)
    );
    // earlier slots untouched
    assert_eq!((cfg.slots[0].width, cfg.slots[0].height), (728, 90));
    let res = send(&mut svm, &[ix_add_slot(&o, &g.program, 1, 1)], &[&g.owner]);
    assert_custom_err(&res, code(E::TooManySlots));
}

#[test]
fn add_slot_rejects_zero_size() {
    let mut svm = setup();
    let (g, _) = new_configured_game(&mut svm);
    let res = send(
        &mut svm,
        &[ix_add_slot(&g.owner.pubkey(), &g.program, 0, 10)],
        &[&g.owner],
    );
    assert_custom_err(&res, code(E::InvalidSlot));
}

#[test]
fn set_slot_active_toggles_and_bounds_checks() {
    let mut svm = setup();
    let (g, _) = new_configured_game(&mut svm);
    let o = g.owner.pubkey();
    let res = send(
        &mut svm,
        &[ix_set_slot_active(&o, &g.program, 1, false)],
        &[&g.owner],
    );
    assert_ok(&res);
    let ev: Vec<SlotActiveSet> = events(&res);
    assert_eq!(
        (ev[0].game_program, ev[0].slot_index, ev[0].active),
        (g.program, 1, false)
    );
    let cfg: GameAdConfig = fetch(&svm, &ad_config_pda(&g.program));
    assert!(cfg.slots[0].active && !cfg.slots[1].active);
    assert_eq!(
        (cfg.slots[1].width, cfg.slots[1].height),
        (300, 250),
        "size kept"
    );
    assert_ok(&send(
        &mut svm,
        &[ix_set_slot_active(&o, &g.program, 1, true)],
        &[&g.owner],
    ));
    let res = send(
        &mut svm,
        &[ix_set_slot_active(&o, &g.program, 2, true)],
        &[&g.owner],
    );
    assert_custom_err(&res, code(E::InvalidSlot));
}

// ------------------------------------------------------------------ IDL

#[test]
fn idl_has_game_ad_config_with_fixed_slot_array() {
    let path = format!(
        "{}/../../target/idl/ad_pool.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let idl: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let accounts = idl["accounts"]
        .as_array()
        .expect("IDL accounts present once an ix references them");
    assert!(accounts.iter().any(|a| a["name"] == "GameAdConfig"));
    let ty = idl["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "GameAdConfig")
        .unwrap();
    let slots = ty["type"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "slots")
        .unwrap();
    assert_eq!(
        slots["type"],
        serde_json::json!({"array": [{"defined": {"name": "AdSlotSpec"}}, 8]})
    );
}
