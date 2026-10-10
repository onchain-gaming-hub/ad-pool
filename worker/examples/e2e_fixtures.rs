//! W5 e2e: account bytes for the mock RPC, built from the **program's own types** (ad_pool +
//! `declare_program!(platform_core)`), padded to the allocated size like the real accounts.
//!   cargo run -q --example e2e_fixtures -- <now> <advertiser> <admin> <moderator> <game_owner>
//! Prints JSON `{ "game": …, "accounts": { address: { owner, lamports, data(base64) } }, "auctions": { name: address } }`.
use std::collections::BTreeMap;

use ad_pool::{
    platform_core::{
        self,
        accounts::{DefaultAd, GameEntry, RoleMember},
        types::{AdCreative as PcCreative, GameStatus, Role},
    },
    state::{AdCreative, AdSlotAuction, CreativeStatus},
};
use ad_pool_worker::{accounts as w, pda};
use anchor_lang::{AccountSerialize, prelude::Pubkey as APubkey};
use base64::Engine;
use solana_pubkey::Pubkey;

fn ser<T: AccountSerialize>(v: &T, size: usize) -> Vec<u8> {
    let mut b = Vec::new();
    v.try_serialize(&mut b).unwrap();
    assert!(b.len() <= size);
    b.resize(size, 0);
    b
}
fn a(k: &Pubkey) -> APubkey {
    APubkey::new_from_array(k.to_bytes())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let now: i64 = args[1].parse().unwrap();
    let key = |i: usize| -> Pubkey { args[i].parse().unwrap() };
    let (adv, admin, moderator, game_owner) = (key(2), key(3), key(4), key(5));
    let ad_pool_id = Pubkey::new_from_array(ad_pool::ID.to_bytes());
    let pc_id = Pubkey::new_from_array(platform_core::ID.to_bytes());
    let game = Pubkey::new_from_array([0x47; 32]);

    let mut accounts = BTreeMap::new();
    let mut names = BTreeMap::new();
    let mut put = |addr: Pubkey, owner: Pubkey, data: Vec<u8>| {
        accounts.insert(
            addr.to_string(),
            serde_json::json!({
                "owner": owner.to_string(),
                "lamports": 10_000_000u64,
                "data": base64::engine::general_purpose::STANDARD.encode(data),
            }),
        );
    };

    let day = 86_400;
    let mut auction = |name: &str,
                       epoch: u64,
                       status: CreativeStatus,
                       settled: bool,
                       close: i64,
                       start: i64,
                       click: &str| {
        let addr = pda::ad_slot(&ad_pool_id, &game, epoch, 0);
        let (image_url, sha) = if status == CreativeStatus::Approved {
            ("https://ads.e2e/ads/x.png".to_string(), [7u8; 32])
        } else {
            (String::new(), [0u8; 32])
        };
        let creative = AdCreative {
            image_url,
            image_sha256: sha,
            text: if status == CreativeStatus::Empty {
                String::new()
            } else {
                "ข้อความ".into()
            },
            click_url: click.into(),
        };
        let v = AdSlotAuction {
            version: 1,
            bump: 255,
            creative_status: status,
            settled,
            slot_index: 0,
            game_program: a(&game),
            epoch,
            width: 728,
            height: 90,
            close_ts: close,
            epoch_start_ts: start,
            epoch_end_ts: start + day,
            fee_bps: 100,
            min_increment_bps: 1000,
            rent_payer: a(&adv),
            highest_bidder: a(&adv),
            highest_bid: 20_000_000,
            bid_count: 1,
            reject_count: 0,
            last_reject_flags: 0,
            approved_at: 0,
            updated_at: now,
            _reserved: [0; 32],
            creative,
        };
        put(addr, ad_pool_id, ser(&v, w::AD_SLOT_AUCTION_SIZE));
        names.insert(name.to_string(), (addr.to_string(), epoch));
    };
    // creative window open: close passed, epoch in 3 days
    let (wc, ws) = (now - 3_600, now + 3 * day);
    for e in 1..=20u64 {
        auction(
            &format!("empty{e}"),
            e,
            CreativeStatus::Empty,
            false,
            wc,
            ws,
            "",
        );
    }
    auction(
        "textonly1",
        21,
        CreativeStatus::TextOnly,
        false,
        wc,
        ws,
        "https://example.com/",
    );
    auction(
        "textonly2",
        22,
        CreativeStatus::TextOnly,
        false,
        wc,
        ws,
        "https://example.com/",
    );
    auction(
        "bidding",
        30,
        CreativeStatus::Empty,
        false,
        now + 3_600,
        now + 8 * day,
        "",
    );
    // crank
    auction(
        "settle_due",
        40,
        CreativeStatus::Empty,
        false,
        now - 8 * day,
        now - 100,
        "",
    );
    auction(
        "settle_close_due",
        41,
        CreativeStatus::Empty,
        false,
        now - 9 * day,
        now - day - 100,
        "",
    );
    auction(
        "close_due",
        42,
        CreativeStatus::Approved,
        true,
        now - 9 * day,
        now - day - 100,
        "https://example.com/",
    );
    auction(
        "revoke_me",
        43,
        CreativeStatus::Approved,
        false,
        wc,
        ws,
        "https://malware.e2e-test.example.org/",
    );
    auction(
        "stale",
        44,
        CreativeStatus::Empty,
        false,
        now - 9 * day,
        now - 7 * 3_600,
        "",
    );

    // platform-core: GameEntry, roles, DefaultAd 300×250 (exists, disabled)
    let ge = GameEntry {
        version: 1,
        bump: 255,
        status: GameStatus::Active,
        game_program: a(&game),
        owner: a(&game_owner),
        game_authority: a(&Pubkey::new_from_array([1; 32])),
        game_authority_bump: 255,
        registered_by: a(&admin),
        registered_at: 1,
        updated_at: 1,
        _reserved: [0; 64],
    };
    put(pda::game_entry(&pc_id, &game), pc_id, ser(&ge, 220));
    for (role, member) in [(Role::Moderator, moderator), (Role::Admin, admin)] {
        let wr = if matches!(role, Role::Admin) {
            w::Role::Admin
        } else {
            w::Role::Moderator
        };
        let rm = RoleMember {
            version: 1,
            bump: 255,
            role,
            member: a(&member),
            granted_by: a(&admin),
            granted_at: 1,
            _reserved: [0; 32],
        };
        put(pda::role_member(&pc_id, wr, &member), pc_id, ser(&rm, 115));
    }
    let da = DefaultAd {
        version: 1,
        bump: 255,
        width: 300,
        height: 250,
        enabled: false,
        updated_by: a(&moderator),
        updated_at: 1,
        creative: PcCreative {
            image_url: "https://ads.e2e/ads/old.png".into(),
            image_sha256: [3; 32],
            text: "เก่า".into(),
            click_url: "https://example.com/".into(),
        },
        _reserved: [0; 32],
    };
    put(
        pda::default_ad(&pc_id, 300, 250),
        pc_id,
        ser(&da, w::DEFAULT_AD_SIZE),
    );

    println!(
        "{}",
        serde_json::json!({
            "game": game.to_string(),
            "game_owner": game_owner.to_string(),
            "ad_pool": ad_pool_id.to_string(),
            "platform_core": pc_id.to_string(),
            "default_ad_728x90": pda::default_ad(&pc_id, 728, 90).to_string(),
            "default_ad_300x250": pda::default_ad(&pc_id, 300, 250).to_string(),
            "treasury": pda::treasury(&pc_id).to_string(),
            "accounts": accounts,
            "auctions": names,
        })
    );
}
