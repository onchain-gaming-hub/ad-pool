//! P3 — moderator instructions (design §7 · §4.1 · §14 "role / A13 / moderator / revoke / tx size").
#![allow(clippy::result_large_err)]

mod common;

use {
    ad_pool::{
        constants::*,
        error::AdPoolError as E,
        events::{CreativeApproved, CreativeRejected, CreativeRevoked},
        platform_core::types::{GameStatus, Role},
        state::{AdCreative, CreativeStatus},
    },
    anchor_lang::{prelude::Pubkey, solana_program::instruction::Instruction},
    common::*,
    litesvm::{types::TransactionResult, LiteSVM},
    solana_keypair::Keypair,
    solana_message::Message,
    solana_signer::Signer,
};

const E8: u64 = FIRST_OPEN_EPOCH;

/// Configured game + an open auction on (E8, slot 0) + a Moderator; clock at close_ts.
struct World {
    svm: LiteSVM,
    game: Game,
    auction: Pubkey,
    moderator: Keypair,
    close_ts: i64,
    end_ts: i64,
}

fn world() -> World {
    let mut svm = setup();
    let (game, a) = new_configured_game(&mut svm);
    let alice = funded(&mut svm);
    assert_ok(&send(
        &mut svm,
        &[ix_open_auction(
            &alice.pubkey(),
            &game.program,
            E8,
            0,
            a.min_bid,
        )],
        &[&alice],
    ));
    let au = auction_of(&svm, &game.program, E8, 0);
    let moderator = new_moderator(&mut svm);
    set_time(&mut svm, au.close_ts);
    World {
        svm,
        auction: ad_slot_pda(&game.program, E8, 0),
        game,
        moderator,
        close_ts: au.close_ts,
        end_ts: au.epoch_end_ts,
    }
}

impl World {
    fn run(&mut self, ix: Instruction) -> TransactionResult {
        let m = self.moderator.insecure_clone();
        send(&mut self.svm, &[ix], &[&m])
    }
    fn approve(&mut self, c: AdCreative, flags: u32) -> TransactionResult {
        let ix = ix_approve_creative(&self.moderator.pubkey(), &self.auction, c, flags);
        self.run(ix)
    }
    fn approve_image(&mut self, url: &str, sha: [u8; 32]) -> TransactionResult {
        let ix = ix_approve_image(&self.moderator.pubkey(), &self.auction, url.into(), sha);
        self.run(ix)
    }
    fn reject(&mut self, flags: u32) -> TransactionResult {
        let ix = ix_reject_creative(&self.moderator.pubkey(), &self.auction, flags);
        self.run(ix)
    }
    fn revoke(&mut self, flags: u32) -> TransactionResult {
        let ix = ix_revoke_creative(&self.moderator.pubkey(), &self.auction, flags);
        self.run(ix)
    }
    fn state(&self) -> ad_pool::state::AdSlotAuction {
        fetch(&self.svm, &self.auction)
    }
}

// ------------------------------------------------------------- approve full

#[test]
fn approve_full_creative() {
    let mut w = world();
    let res = w.approve(full_creative(), 0);
    assert_ok(&res);
    let au = w.state();
    assert_eq!(au.creative_status, CreativeStatus::Approved);
    assert_eq!(au.creative, full_creative());
    assert_eq!(
        (au.approved_at, au.updated_at, au.reject_count),
        (w.close_ts, w.close_ts, 0)
    );
    let ev: Vec<CreativeApproved> = events(&res);
    assert_eq!(ev.len(), 1);
    let e = &ev[0];
    assert_eq!(
        (e.moderator, e.game_program, e.epoch, e.slot_index),
        (w.moderator.pubkey(), w.game.program, E8, 0)
    );
    assert_eq!(
        (e.status, e.image_sha256, e.image_reject_flags),
        (CreativeStatus::Approved, [0xAB; 32], 0)
    );
}

#[test]
fn approved_is_final() {
    let mut w = world();
    assert_ok(&w.approve(full_creative(), 0));
    assert_custom_err(
        &w.approve(text_only_creative(), 0),
        code(E::CreativeAlreadyFinal),
    );
    assert_custom_err(
        &w.approve_image("https://x/y.png", [1; 32]),
        code(E::NotTextOnly),
    );
    assert_custom_err(
        &w.reject(REJECT_IMAGE_MODERATION),
        code(E::CreativeAlreadyFinal),
    );
    assert_eq!(w.state().creative, full_creative(), "unchanged");
}

#[test]
fn text_only_text_and_click_are_locked() {
    // Q4 / §7.5: once text + click_url are approved, approve_creative cannot replace them.
    let mut w = world();
    assert_ok(&w.approve(text_only_creative(), 0));
    let other = AdCreative {
        text: "อื่น".into(),
        click_url: "https://evil.example".into(),
        ..full_creative()
    };
    assert_custom_err(&w.approve(other, 0), code(E::CreativeAlreadyFinal));
    assert_custom_err(
        &w.approve(text_only_creative(), 0),
        code(E::CreativeAlreadyFinal),
    );
    assert_eq!(w.state().creative, text_only_creative());
}

// ------------------------------------------------------------- A13 text-only

#[test]
fn text_only_then_image_reaches_approved_and_keeps_text() {
    let mut w = world();
    let res = w.approve(text_only_creative(), REJECT_IMAGE_MODERATION);
    assert_ok(&res);
    let au = w.state();
    assert_eq!(au.creative_status, CreativeStatus::TextOnly);
    assert_eq!(
        (au.reject_count, au.last_reject_flags),
        (1, REJECT_IMAGE_MODERATION),
        "image rejection recorded in the same tx"
    );
    let ev: Vec<CreativeApproved> = events(&res);
    assert_eq!(
        (ev[0].status, ev[0].image_reject_flags),
        (CreativeStatus::TextOnly, REJECT_IMAGE_MODERATION)
    );

    // a second bad image
    assert_ok(&w.reject(REJECT_DIMENSIONS));
    assert_eq!(
        (w.state().reject_count, w.state().last_reject_flags),
        (2, REJECT_DIMENSIONS)
    );
    assert_eq!(w.state().creative_status, CreativeStatus::TextOnly);

    set_time(&mut w.svm, w.close_ts + 3_600);
    let res = w.approve_image("https://ads.example.r2.dev/ads/cd.png", [0xCD; 32]);
    assert_ok(&res);
    let au = w.state();
    assert_eq!(au.creative_status, CreativeStatus::Approved);
    assert_eq!(
        au.creative.image_url,
        "https://ads.example.r2.dev/ads/cd.png"
    );
    assert_eq!(au.creative.image_sha256, [0xCD; 32]);
    assert_eq!(
        (au.creative.text.as_str(), au.creative.click_url.as_str()),
        (
            full_creative().text.as_str(),
            full_creative().click_url.as_str()
        )
    );
    assert_eq!(au.approved_at, w.close_ts + 3_600);
    let ev: Vec<CreativeApproved> = events(&res);
    assert_eq!(
        (ev[0].status, ev[0].image_sha256, ev[0].image_reject_flags),
        (CreativeStatus::Approved, [0xCD; 32], 0)
    );
}

#[test]
fn text_only_without_flags_does_not_count_a_reject() {
    let mut w = world();
    assert_ok(&w.approve(text_only_creative(), 0));
    assert_eq!(
        (w.state().reject_count, w.state().last_reject_flags),
        (0, 0)
    );
}

#[test]
fn approve_image_validation() {
    let mut w = world();
    assert_custom_err(
        &w.approve_image("https://x/y.png", [1; 32]),
        code(E::NotTextOnly),
    ); // Empty
    assert_ok(&w.approve(text_only_creative(), 0));
    assert_custom_err(
        &w.approve_image("https://x/y.png", [0; 32]),
        code(E::InvalidImageHash),
    );
    assert_custom_err(&w.approve_image("", [1; 32]), code(E::InvalidCreativeShape));
    let long = format!("https://{}", "a".repeat(AD_IMAGE_URL_MAX_LEN as usize - 7));
    assert_eq!(long.len(), AD_IMAGE_URL_MAX_LEN as usize + 1);
    assert_custom_err(
        &w.approve_image(&long, [1; 32]),
        code(E::CreativeFieldTooLong),
    );
    let max = &long[..AD_IMAGE_URL_MAX_LEN as usize];
    assert_ok(&w.approve_image(max, [1; 32]));
}

// --------------------------------------------------------------- shapes

#[test]
fn approve_rejects_bad_shapes_and_fields() {
    let mut w = world();
    let flags = |c: AdCreative, f: u32| (c, f);
    let half_url = AdCreative {
        image_sha256: [0; 32],
        ..full_creative()
    };
    let half_hash = AdCreative {
        image_url: String::new(),
        ..full_creative()
    };
    let thai_121 = format!("{}x", "ก".repeat(40)); // 40 × 3 bytes + 1 = 121 bytes
    assert_eq!(thai_121.len(), 121);
    let cases: Vec<((AdCreative, u32), u32)> = vec![
        (flags(half_url, 0), code(E::InvalidCreativeShape)),
        (flags(half_hash, 0), code(E::InvalidCreativeShape)),
        (
            flags(full_creative(), REJECT_IMAGE_MODERATION),
            code(E::InvalidCreativeShape),
        ),
        (
            flags(
                AdCreative {
                    text: String::new(),
                    ..full_creative()
                },
                0,
            ),
            code(E::InvalidCreativeShape),
        ),
        (
            flags(
                AdCreative {
                    click_url: String::new(),
                    ..full_creative()
                },
                0,
            ),
            code(E::InvalidCreativeShape),
        ),
        (
            flags(
                AdCreative {
                    text: thai_121,
                    ..full_creative()
                },
                0,
            ),
            code(E::CreativeFieldTooLong),
        ),
        (
            flags(
                AdCreative {
                    click_url: format!("https://{}", "a".repeat(193)),
                    ..full_creative()
                },
                0,
            ),
            code(E::CreativeFieldTooLong),
        ),
        (
            flags(
                AdCreative {
                    image_url: "u".repeat(201),
                    ..full_creative()
                },
                0,
            ),
            code(E::CreativeFieldTooLong),
        ),
        (
            flags(
                AdCreative {
                    click_url: "http://example.com".into(),
                    ..full_creative()
                },
                0,
            ),
            code(E::InvalidClickUrl),
        ),
        (
            flags(
                AdCreative {
                    click_url: "javascript:alert(1)".into(),
                    ..full_creative()
                },
                0,
            ),
            code(E::InvalidClickUrl),
        ),
        (
            flags(
                AdCreative {
                    click_url: "https://".into(),
                    ..full_creative()
                },
                0,
            ),
            code(E::InvalidClickUrl),
        ),
        (
            flags(
                AdCreative {
                    click_url: "HTTPS://example.com".into(),
                    ..full_creative()
                },
                0,
            ),
            code(E::InvalidClickUrl),
        ),
        (
            flags(
                AdCreative {
                    click_url: "https://a.com/ x".into(),
                    ..full_creative()
                },
                0,
            ),
            code(E::InvalidClickUrl),
        ),
        (
            flags(
                AdCreative {
                    click_url: "https://a.com/\nx".into(),
                    ..full_creative()
                },
                0,
            ),
            code(E::InvalidClickUrl),
        ),
    ];
    for ((c, f), err) in cases {
        assert_custom_err(&w.approve(c, f), err);
    }
    assert_eq!(w.state().creative_status, CreativeStatus::Empty);
    // exactly-max fields fit the account
    let max = AdCreative {
        image_url: "u".repeat(200),
        image_sha256: [9; 32],
        text: "ก".repeat(40),
        click_url: format!("https://{}", "a".repeat(192)),
    };
    assert_eq!((max.text.len(), max.click_url.len()), (120, 200));
    assert_ok(&w.approve(max.clone(), 0));
    assert_eq!(w.state().creative, max);
}

// ------------------------------------------------------------- reject / revoke

#[test]
fn reject_keeps_status_and_records_reason() {
    let mut w = world();
    let res = w.reject(REJECT_FILE_TYPE | REJECT_TOO_LONG);
    assert_ok(&res);
    let au = w.state();
    assert_eq!(
        (au.creative_status, au.reject_count, au.last_reject_flags),
        (CreativeStatus::Empty, 1, REJECT_FILE_TYPE | REJECT_TOO_LONG)
    );
    let ev: Vec<CreativeRejected> = events(&res);
    assert_eq!(
        (ev[0].moderator, ev[0].flags, ev[0].reject_count),
        (w.moderator.pubkey(), REJECT_FILE_TYPE | REJECT_TOO_LONG, 1)
    );
    // winner resubmits and passes (A8)
    assert_ok(&w.approve(full_creative(), 0));
}

#[test]
fn revoke_returns_to_empty_and_allows_resubmission() {
    let mut w = world();
    assert_custom_err(&w.revoke(REJECT_URL_REPUTATION), code(E::NothingToRevoke));
    assert_ok(&w.approve(full_creative(), 0));
    let res = w.revoke(REJECT_URL_REPUTATION);
    assert_ok(&res);
    let au = w.state();
    assert_eq!(au.creative_status, CreativeStatus::Empty);
    assert_eq!(au.creative, AdCreative::default());
    assert_eq!(
        (au.approved_at, au.reject_count, au.last_reject_flags),
        (0, 1, REJECT_URL_REPUTATION)
    );
    let ev: Vec<CreativeRevoked> = events(&res);
    assert_eq!(
        (ev[0].moderator, ev[0].prev_status, ev[0].flags),
        (
            w.moderator.pubkey(),
            CreativeStatus::Approved,
            REJECT_URL_REPUTATION
        )
    );
    // revoke of TextOnly too
    assert_ok(&w.approve(text_only_creative(), 0));
    assert_ok(&w.revoke(REJECT_TEXT_MODERATION));
    assert_eq!(w.state().creative_status, CreativeStatus::Empty);
    // approve_image needs TextOnly — not Empty after a revoke
    assert_custom_err(
        &w.approve_image("https://x/y.png", [1; 32]),
        code(E::NotTextOnly),
    );
    // resubmission after revoke
    assert_ok(&w.approve(full_creative(), 0));
    assert_eq!(w.state().creative_status, CreativeStatus::Approved);
}

#[test]
fn moderation_moves_no_lamports() {
    let mut w = world();
    let before = lamports(&w.svm, &w.auction);
    assert_ok(&w.approve(text_only_creative(), REJECT_IMAGE_MODERATION));
    assert_ok(&w.reject(REJECT_DIMENSIONS));
    assert_ok(&w.approve_image("https://x/y.png", [1; 32]));
    assert_ok(&w.revoke(REJECT_URL_RULES));
    assert_eq!(lamports(&w.svm, &w.auction), before);
}

// ---------------------------------------------------------------- window

#[test]
fn creative_window_is_close_to_epoch_end() {
    let mut w = world();
    set_time(&mut w.svm, w.close_ts - 1);
    assert_custom_err(&w.approve(full_creative(), 0), code(E::NotInCreativeWindow));
    assert_custom_err(&w.reject(1), code(E::NotInCreativeWindow));
    set_time(&mut w.svm, w.end_ts);
    assert_custom_err(&w.approve(full_creative(), 0), code(E::NotInCreativeWindow));
    assert_custom_err(&w.revoke(1), code(E::NotInCreativeWindow));
    assert_custom_err(&w.reject(1), code(E::NotInCreativeWindow));
    assert_custom_err(
        &w.approve_image("https://x/y.png", [1; 32]),
        code(E::NotInCreativeWindow),
    );
    set_time(&mut w.svm, w.close_ts - 1);
    assert_custom_err(
        &w.approve_image("https://x/y.png", [1; 32]),
        code(E::NotInCreativeWindow),
    );
    assert_custom_err(&w.revoke(1), code(E::NotInCreativeWindow));
    // Live (after epoch start) is still allowed (Q5)
    set_time(&mut w.svm, w.end_ts - 1);
    assert_ok(&w.approve(text_only_creative(), 0));
    assert_ok(&w.approve_image("https://x/y.png", [1; 32]));
    assert_ok(&w.revoke(1));
}

// ------------------------------------------------------------------ role

#[test]
fn admin_is_not_moderator() {
    let mut w = world();
    let admin = funded(&mut w.svm);
    put_role(&mut w.svm, Role::Admin, &admin.pubkey());
    // Admin's own RoleMember PDA does not match the Moderator seeds
    let ix = Instruction::new_with_bytes(
        ad_pool::id(),
        &anchor_lang::InstructionData::data(&ad_pool::instruction::RejectCreative { flags: 1 }),
        moderate_metas(
            &admin.pubkey(),
            &role_pda(Role::Admin, &admin.pubkey()).0,
            &w.auction,
        ),
    );
    assert_custom_err(&send(&mut w.svm, &[ix], &[&admin]), 2006);
    // and the Moderator PDA for the admin does not exist
    let ix = ix_reject_creative(&admin.pubkey(), &w.auction, 1);
    assert_custom_err(&send(&mut w.svm, &[ix], &[&admin]), 3012);
}

#[test]
fn someone_elses_moderator_role_cannot_be_borrowed() {
    let mut w = world();
    let stranger = funded(&mut w.svm);
    let ix = Instruction::new_with_bytes(
        ad_pool::id(),
        &anchor_lang::InstructionData::data(&ad_pool::instruction::RejectCreative { flags: 1 }),
        moderate_metas(
            &stranger.pubkey(),
            &role_pda(Role::Moderator, &w.moderator.pubkey()).0,
            &w.auction,
        ),
    );
    assert_custom_err(&send(&mut w.svm, &[ix], &[&stranger]), 2006);
}

#[test]
fn revoked_role_fails_closed() {
    let mut w = world();
    let (role, _) = role_pda(Role::Moderator, &w.moderator.pubkey());
    w.svm
        .set_account(role, solana_account::Account::default())
        .unwrap(); // closed by platform-core
    assert_custom_err(&w.approve(full_creative(), 0), 3012);
}

#[test]
fn fake_role_account_is_rejected() {
    let mut w = world();
    let (role, _) = role_pda(Role::Moderator, &w.moderator.pubkey());
    let mut acct = w.svm.get_account(&role).unwrap();
    acct.owner = Pubkey::new_unique();
    w.svm.set_account(role, acct).unwrap();
    assert_custom_err(&w.approve(full_creative(), 0), 3007);
}

#[test]
fn moderation_works_on_suspended_game() {
    let mut w = world();
    put_game_entry(
        &mut w.svm,
        &w.game.program,
        &w.game.owner.pubkey(),
        GameStatus::Suspended,
    );
    assert_ok(&w.approve(full_creative(), 0));
}

#[test]
fn fake_auction_is_rejected() {
    let mut w = world();
    let mut acct = w.svm.get_account(&w.auction).unwrap();
    acct.owner = Pubkey::new_unique();
    let fake = Pubkey::new_unique();
    w.svm.set_account(fake, acct).unwrap();
    let ix = ix_approve_creative(&w.moderator.pubkey(), &fake, full_creative(), 0);
    assert_custom_err(&w.run(ix), 3007);
}

// --------------------------------------------------------------- tx size

#[test]
fn approve_creative_at_max_length_fits_in_one_transaction() {
    // design §9: 857 B with full strings + a SetComputeUnitPrice ix.
    let moderator = Pubkey::new_unique();
    let max = AdCreative {
        image_url: "u".repeat(200),
        image_sha256: [9; 32],
        text: "t".repeat(120),
        click_url: format!("https://{}", "a".repeat(192)),
    };
    let approve = ix_approve_creative(&moderator, &Pubkey::new_unique(), max, 0);
    let mut price = vec![3u8]; // ComputeBudgetInstruction::SetComputeUnitPrice
    price.extend_from_slice(&1_000u64.to_le_bytes());
    let cb: Pubkey = "ComputeBudget111111111111111111111111111111"
        .parse()
        .unwrap();
    let price_ix = Instruction::new_with_bytes(cb, &price, vec![]);
    let msg = Message::new(&[price_ix, approve], Some(&moderator));
    let tx_len = 1 + 64 + msg.serialize().len();
    assert_eq!(tx_len, 857, "design §9");
    assert!(tx_len <= 1232);
}

#[test]
fn revoke_zeroes_the_old_creative_bytes() {
    // Raw-byte scanners must not find a revoked (possibly malicious) URL.
    let mut w = world();
    assert_ok(&w.approve(full_creative(), 0));
    assert_ok(&w.revoke(REJECT_URL_REPUTATION));
    let data = w.svm.get_account(&w.auction).unwrap().data;
    assert_eq!(data.len(), AD_SLOT_AUCTION_SIZE as usize);
    assert!(
        data[CREATIVE_OFFSET as usize..].iter().all(|b| *b == 0),
        "creative tail fully zeroed"
    );
}
