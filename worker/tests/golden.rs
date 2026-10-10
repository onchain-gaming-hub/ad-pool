//! Golden tests (native): every Worker decoder / PDA / constant must agree with the real
//! program types (`ad_pool` crate via dev-dependency) and the exported IDLs.

use {
    ad_pool::{
        platform_core,
        state::{AdCreative, AdSlotAuction, AdSlotSpec, CreativeStatus, GameAdConfig},
    },
    ad_pool_worker::{accounts as w, pda, phase},
    anchor_lang::{AccountSerialize, prelude::Pubkey as APubkey},
    solana_pubkey::Pubkey,
};

fn ser<T: AccountSerialize>(v: &T) -> Vec<u8> {
    let mut b = Vec::new();
    v.try_serialize(&mut b).unwrap();
    b
}

fn ak(b: u8) -> APubkey {
    APubkey::new_from_array([b; 32])
}
fn wk(b: u8) -> Pubkey {
    Pubkey::new_from_array([b; 32])
}
fn to_w(k: &APubkey) -> Pubkey {
    Pubkey::new_from_array(k.to_bytes())
}

fn idl(name: &str) -> serde_json::Value {
    let p = format!("{}/../idls/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn idl_disc(idl: &serde_json::Value, account: &str) -> [u8; 8] {
    let a = idl["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == account)
        .unwrap();
    let v: Vec<u8> = a["discriminator"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap() as u8)
        .collect();
    v.try_into().unwrap()
}

fn idl_const(idl: &serde_json::Value, name: &str) -> String {
    idl["constants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap()["value"]
        .as_str()
        .unwrap()
        .to_string()
}

fn sample_auction(creative: AdCreative, status: CreativeStatus) -> AdSlotAuction {
    AdSlotAuction {
        version: 1,
        bump: 253,
        creative_status: status,
        settled: true,
        slot_index: 7,
        game_program: ak(0x22),
        epoch: 0x0102_0304_0506_0708,
        width: 728,
        height: 90,
        close_ts: 1_000,
        epoch_start_ts: 2_000,
        epoch_end_ts: 3_000,
        fee_bps: 100,
        min_increment_bps: 500,
        rent_payer: ak(0x33),
        highest_bidder: ak(0x44),
        highest_bid: 0x6162_6364_6566_6768,
        bid_count: 9,
        reject_count: 3,
        last_reject_flags: 0x24,
        approved_at: 1_500,
        updated_at: 1_600,
        _reserved: [0; 32],
        creative,
    }
}

fn thai_creative() -> AdCreative {
    AdCreative {
        image_url: "https://ads.example.r2.dev/ads/ab.png".into(),
        image_sha256: [0xAB; 32],
        text: "โฆษณาทดสอบ ภาษาไทย".into(),
        click_url: "https://example.com/promo".into(),
    }
}

#[test]
fn discriminators_match_idls() {
    let ad = idl("ad_pool");
    let pc = idl("platform_core");
    assert_eq!(
        w::AD_SLOT_AUCTION_DISCRIMINATOR,
        idl_disc(&ad, "AdSlotAuction")
    );
    assert_eq!(
        w::GAME_AD_CONFIG_DISCRIMINATOR,
        idl_disc(&ad, "GameAdConfig")
    );
    assert_eq!(w::GAME_ENTRY_DISCRIMINATOR, idl_disc(&pc, "GameEntry"));
    assert_eq!(
        w::PLATFORM_CONFIG_DISCRIMINATOR,
        idl_disc(&pc, "PlatformConfig")
    );
    assert_eq!(w::ROLE_MEMBER_DISCRIMINATOR, idl_disc(&pc, "RoleMember"));
}

#[test]
fn constants_match_ad_pool_idl() {
    let ad = idl("ad_pool");
    assert_eq!(
        idl_const(&ad, "AD_SLOT_AUCTION_SIZE"),
        w::AD_SLOT_AUCTION_SIZE.to_string()
    );
    assert_eq!(
        idl_const(&ad, "GAME_AD_CONFIG_SIZE"),
        w::GAME_AD_CONFIG_SIZE.to_string()
    );
    assert_eq!(
        idl_const(&ad, "CREATIVE_OFFSET"),
        w::CREATIVE_OFFSET.to_string()
    );
    assert_eq!(idl_const(&ad, "MAX_SLOTS"), w::MAX_SLOTS.to_string());
    assert_eq!(
        idl_const(&ad, "AD_IMAGE_URL_MAX_LEN"),
        w::AD_IMAGE_URL_MAX_LEN.to_string()
    );
    assert_eq!(
        idl_const(&ad, "AD_TEXT_MAX_LEN"),
        w::AD_TEXT_MAX_LEN.to_string()
    );
    assert_eq!(
        idl_const(&ad, "AD_CLICK_URL_MAX_LEN"),
        w::AD_CLICK_URL_MAX_LEN.to_string()
    );
    assert_eq!(
        idl_const(&ad, "ACCOUNT_VERSION"),
        w::ACCOUNT_VERSION.to_string()
    );
    assert_eq!(
        idl_const(&ad, "AD_CONFIG_SEED"),
        format!("{:?}", pda::AD_CONFIG_SEED)
    );
    assert_eq!(
        idl_const(&ad, "AD_SLOT_SEED"),
        format!("{:?}", pda::AD_SLOT_SEED)
    );
    assert_eq!(ad["address"].as_str().unwrap(), ad_pool::ID.to_string());
}

#[test]
fn decodes_ad_slot_auction_written_by_the_program_types() {
    for (creative, status) in [
        (thai_creative(), CreativeStatus::Approved),
        (AdCreative::default(), CreativeStatus::Empty),
        (
            AdCreative {
                image_url: String::new(),
                image_sha256: [0; 32],
                ..thai_creative()
            },
            CreativeStatus::TextOnly,
        ),
        (
            AdCreative {
                image_url: "u".repeat(200),
                image_sha256: [9; 32],
                text: "ก".repeat(40),
                click_url: format!("https://{}", "a".repeat(192)),
            },
            CreativeStatus::Approved,
        ),
    ] {
        let a = sample_auction(creative.clone(), status);
        let mut bytes = ser(&a);
        bytes.resize(w::AD_SLOT_AUCTION_SIZE, 0); // on-chain accounts are always 779 B
        let d = w::decode_ad_slot_auction(&bytes).unwrap();
        let ws = match status {
            CreativeStatus::Empty => w::CreativeStatus::Empty,
            CreativeStatus::TextOnly => w::CreativeStatus::TextOnly,
            CreativeStatus::Approved => w::CreativeStatus::Approved,
        };
        assert_eq!(d.creative_status, ws);
        assert_eq!((d.bump, d.settled, d.slot_index), (253, true, 7));
        assert_eq!(d.game_program, to_w(&a.game_program));
        assert_eq!((d.epoch, d.width, d.height), (a.epoch, 728, 90));
        assert_eq!(
            (d.close_ts, d.epoch_start_ts, d.epoch_end_ts),
            (1_000, 2_000, 3_000)
        );
        assert_eq!((d.fee_bps, d.min_increment_bps), (100, 500));
        assert_eq!((d.rent_payer, d.highest_bidder), (wk(0x33), wk(0x44)));
        assert_eq!(
            (
                d.highest_bid,
                d.bid_count,
                d.reject_count,
                d.last_reject_flags
            ),
            (a.highest_bid, 9, 3, 0x24)
        );
        assert_eq!((d.approved_at, d.updated_at), (1_500, 1_600));
        assert_eq!(d.creative.image_url, creative.image_url);
        assert_eq!(d.creative.image_sha256, creative.image_sha256);
        assert_eq!(d.creative.text, creative.text);
        assert_eq!(d.creative.click_url, creative.click_url);
    }
}

#[test]
fn rejects_malformed_auction_bytes() {
    let good = {
        let mut b = ser(&sample_auction(thai_creative(), CreativeStatus::Approved));
        b.resize(w::AD_SLOT_AUCTION_SIZE, 0);
        b
    };
    let mut bad = good.clone();
    bad[0] ^= 1;
    assert_eq!(
        w::decode_ad_slot_auction(&bad),
        Err(w::DecodeError::BadDiscriminator)
    );
    let mut bad = good.clone();
    bad[8] = 2;
    assert_eq!(
        w::decode_ad_slot_auction(&bad),
        Err(w::DecodeError::UnsupportedVersion(2))
    );
    let mut bad = good.clone();
    bad[10] = 3;
    assert_eq!(
        w::decode_ad_slot_auction(&bad),
        Err(w::DecodeError::BadEnum(3))
    );
    let mut bad = good.clone();
    bad[w::CREATIVE_OFFSET..w::CREATIVE_OFFSET + 4].copy_from_slice(&201u32.to_le_bytes());
    assert_eq!(
        w::decode_ad_slot_auction(&bad),
        Err(w::DecodeError::BadString)
    );
    assert!(matches!(
        w::decode_ad_slot_auction(&good[..100]),
        Err(w::DecodeError::TooShort { .. })
    ));
    // a GameAdConfig is not an auction
    assert_eq!(
        w::decode_ad_slot_auction(&ser(&sample_config())),
        Err(w::DecodeError::TooShort {
            need: 259,
            got: 199
        })
    );
}

fn sample_config() -> GameAdConfig {
    let mut slots = [AdSlotSpec::default(); 8];
    slots[0] = AdSlotSpec {
        width: 728,
        height: 90,
        active: true,
    };
    slots[7] = AdSlotSpec {
        width: 300,
        height: 250,
        active: false,
    };
    GameAdConfig {
        version: 1,
        bump: 254,
        slot_count: 8,
        game_program: ak(0x11),
        genesis_ts: 1_790_000_000,
        epoch_duration_secs: 86_400,
        auction_close_lead_secs: 7 * 86_400,
        min_bid: 6_312_720,
        min_increment_bps: 500,
        bid_horizon_epochs: 30,
        slots,
        created_at: 5,
        updated_at: 6,
        _reserved: [0; 64],
    }
}

#[test]
fn decodes_game_ad_config() {
    let c = sample_config();
    let d = w::decode_game_ad_config(&ser(&c)).unwrap();
    assert_eq!((d.bump, d.slot_count, d.game_program), (254, 8, wk(0x11)));
    assert_eq!(
        (
            d.genesis_ts,
            d.epoch_duration_secs,
            d.auction_close_lead_secs
        ),
        (1_790_000_000, 86_400, 7 * 86_400)
    );
    assert_eq!(
        (d.min_bid, d.min_increment_bps, d.bid_horizon_epochs),
        (6_312_720, 500, 30)
    );
    assert_eq!(
        d.slots[0],
        w::AdSlotSpec {
            width: 728,
            height: 90,
            active: true
        }
    );
    assert_eq!(
        d.slots[7],
        w::AdSlotSpec {
            width: 300,
            height: 250,
            active: false
        }
    );
    assert_eq!((d.created_at, d.updated_at), (5, 6));
}

#[test]
fn decodes_platform_core_accounts() {
    use platform_core::{
        accounts::{GameEntry, PlatformConfig, RoleMember},
        types::{GameStatus, Role},
    };
    let ge = GameEntry {
        version: 1,
        bump: 1,
        status: GameStatus::Suspended,
        game_program: ak(1),
        owner: ak(2),
        game_authority: ak(3),
        game_authority_bump: 4,
        registered_by: ak(5),
        registered_at: 6,
        updated_at: 7,
        _reserved: [0; 64],
    };
    let d = w::decode_game_entry(&ser(&ge)).unwrap();
    assert_eq!(
        (d.status, d.game_program, d.owner),
        (w::GameStatus::Suspended, wk(1), wk(2))
    );
    let rm = RoleMember {
        version: 1,
        bump: 9,
        role: Role::Moderator,
        member: ak(7),
        granted_by: ak(8),
        granted_at: 1,
        _reserved: [0; 32],
    };
    let d = w::decode_role_member(&ser(&rm)).unwrap();
    assert_eq!((d.role, d.member), (w::Role::Moderator, wk(7)));
    let pc = PlatformConfig {
        version: 1,
        bump: 1,
        boost_fee_bps: 100,
        ad_fee_bps: 250,
        admin_count: 3,
        treasury_bump: 251,
        initialized_at: 1,
        updated_at: 2,
        _reserved: [0; 64],
    };
    let d = w::decode_platform_config(&ser(&pc)).unwrap();
    assert_eq!(
        (d.boost_fee_bps, d.ad_fee_bps, d.treasury_bump),
        (100, 250, 251)
    );
}

#[test]
fn pdas_match_the_program() {
    let ad = Pubkey::new_from_array(ad_pool::ID.to_bytes());
    let pc = Pubkey::new_from_array(platform_core::ID.to_bytes());
    let game = APubkey::new_unique();
    let wg = to_w(&game);
    let (a_cfg, _) = APubkey::find_program_address(&[b"ad_config", game.as_ref()], &ad_pool::ID);
    assert_eq!(pda::ad_config(&ad, &wg), to_w(&a_cfg));
    let (a_slot, _) = APubkey::find_program_address(
        &[b"ad_slot", game.as_ref(), &42u64.to_le_bytes(), &[3]],
        &ad_pool::ID,
    );
    assert_eq!(pda::ad_slot(&ad, &wg, 42, 3), to_w(&a_slot));
    let (a_ge, _) = APubkey::find_program_address(&[b"game", game.as_ref()], &platform_core::ID);
    assert_eq!(pda::game_entry(&pc, &wg), to_w(&a_ge));
    let m = APubkey::new_unique();
    let (a_role, _) =
        APubkey::find_program_address(&[b"role", &[2], m.as_ref()], &platform_core::ID);
    assert_eq!(
        pda::role_member(&pc, w::Role::Moderator, &to_w(&m)),
        to_w(&a_role)
    );
    // devnet deployment addresses (platform-core runbook §4)
    assert_eq!(
        pda::platform_config(&pc).to_string(),
        "Be8StYkiu7eBMb6m3TvG7QzMZpFL69bcn1kGmZwSmrJ7"
    );
    assert_eq!(
        pda::treasury(&pc).to_string(),
        "CBcvwWyNbXKpk5GK6VczDBqaZrP1qsvBzetv7Ms5A6pz"
    );
}

#[test]
fn phase_matches_program_helper() {
    let mut bytes = ser(&sample_auction(
        AdCreative::default(),
        CreativeStatus::Empty,
    ));
    bytes.resize(w::AD_SLOT_AUCTION_SIZE, 0);
    let d = w::decode_ad_slot_auction(&bytes).unwrap();
    for now in [999, 1_000, 1_999, 2_000, 2_999, 3_000] {
        let ours = phase::phase(now, &d);
        let theirs = ad_pool::math::phase(now, 1_000, 2_000, 3_000);
        assert_eq!(format!("{ours:?}"), format!("{theirs:?}"), "now = {now}");
        assert_eq!(
            phase::in_creative_window(now, &d),
            (1_000..3_000).contains(&now)
        );
    }
}

#[test]
fn platform_core_constants_match_its_idl() {
    let pc = idl("platform_core");
    assert_eq!(
        idl_const(&pc, "GAME_SEED"),
        format!("{:?}", pda::PC_GAME_SEED)
    );
    assert_eq!(
        idl_const(&pc, "CONFIG_SEED"),
        format!("{:?}", pda::PC_CONFIG_SEED)
    );
    assert_eq!(
        idl_const(&pc, "ROLE_SEED"),
        format!("{:?}", pda::PC_ROLE_SEED)
    );
    assert_eq!(
        idl_const(&pc, "TREASURY_SEED"),
        format!("{:?}", pda::PC_TREASURY_SEED)
    );
    assert_eq!(
        pc["address"].as_str().unwrap(),
        "AJnPSYsn7J2muHfKztRtpZz5kBP94dWkNZLr9aMhu18G"
    );
    // Role byte = variant index in the IDL
    let role = pc["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Role")
        .unwrap();
    let names: Vec<&str> = role["type"]["variants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "Admin",
            "GameRegistrar",
            "Moderator",
            "FeeManager",
            "AllowlistManager"
        ]
    );
    assert_eq!(w::Role::Moderator as u8, 2);
}

fn full_auction_bytes() -> Vec<u8> {
    let mut b = ser(&sample_auction(thai_creative(), CreativeStatus::Approved));
    b.resize(w::AD_SLOT_AUCTION_SIZE, 0);
    b
}

#[test]
fn rejects_bad_strings_in_every_creative_field() {
    let good = full_auction_bytes();
    let url_len = thai_creative().image_url.len();
    let text_off = w::CREATIVE_OFFSET + 4 + url_len + 32;
    let text_len = thai_creative().text.len();
    let click_off = text_off + 4 + text_len;
    // text over cap
    let mut b = good.clone();
    b[text_off..text_off + 4].copy_from_slice(&121u32.to_le_bytes());
    assert_eq!(
        w::decode_ad_slot_auction(&b),
        Err(w::DecodeError::BadString)
    );
    // click_url over cap
    let mut b = good.clone();
    b[click_off..click_off + 4].copy_from_slice(&201u32.to_le_bytes());
    assert_eq!(
        w::decode_ad_slot_auction(&b),
        Err(w::DecodeError::BadString)
    );
    // invalid UTF-8 inside text (split a 3-byte Thai char)
    let mut b = good.clone();
    b[text_off + 4] = 0xFF;
    assert_eq!(
        w::decode_ad_slot_auction(&b),
        Err(w::DecodeError::BadString)
    );
    // length within cap but running past the end of the data
    let mut b = good[..click_off + 4 + 5].to_vec();
    b[click_off..click_off + 4].copy_from_slice(&100u32.to_le_bytes());
    assert!(matches!(
        w::decode_ad_slot_auction(&b),
        Err(w::DecodeError::TooShort { .. })
    ));
}

#[test]
fn rejects_bad_enums_in_other_accounts() {
    let mut c = ser(&sample_config());
    c[79 + 4] = 2; // slots[0].active bool
    assert_eq!(
        w::decode_game_ad_config(&c),
        Err(w::DecodeError::BadEnum(2))
    );
    use platform_core::{
        accounts::{GameEntry, RoleMember},
        types::{GameStatus, Role},
    };
    let mut ge = ser(&GameEntry {
        version: 1,
        bump: 1,
        status: GameStatus::Active,
        game_program: ak(1),
        owner: ak(2),
        game_authority: ak(3),
        game_authority_bump: 4,
        registered_by: ak(5),
        registered_at: 6,
        updated_at: 7,
        _reserved: [0; 64],
    });
    ge[10] = 2;
    assert_eq!(w::decode_game_entry(&ge), Err(w::DecodeError::BadEnum(2)));
    let mut rm = ser(&RoleMember {
        version: 1,
        bump: 9,
        role: Role::Admin,
        member: ak(7),
        granted_by: ak(8),
        granted_at: 1,
        _reserved: [0; 32],
    });
    rm[10] = 5;
    assert_eq!(w::decode_role_member(&rm), Err(w::DecodeError::BadEnum(5)));
    rm[0] ^= 1;
    assert_eq!(
        w::decode_role_member(&rm),
        Err(w::DecodeError::BadDiscriminator)
    );
}

#[test]
fn checked_decoders_require_the_owner_and_the_right_role() {
    use ad_pool_worker::rpc::AccountInfo;
    use platform_core::{accounts::RoleMember, types::Role};
    let ad = Pubkey::new_from_array(ad_pool::ID.to_bytes());
    let pc = Pubkey::new_from_array(platform_core::ID.to_bytes());
    let info = |owner: Pubkey, data: Vec<u8>| AccountInfo {
        lamports: 1,
        owner,
        executable: false,
        data,
    };
    assert!(w::ad_slot_auction(&info(ad, full_auction_bytes()), &ad).is_ok());
    assert_eq!(
        w::ad_slot_auction(&info(wk(9), full_auction_bytes()), &ad),
        Err(w::DecodeError::WrongOwner)
    );
    let rm = ser(&RoleMember {
        version: 1,
        bump: 9,
        role: Role::Moderator,
        member: ak(7),
        granted_by: ak(8),
        granted_at: 1,
        _reserved: [0; 32],
    });
    assert!(w::role_member(&info(pc, rm.clone()), &pc, w::Role::Moderator, &wk(7)).is_ok());
    assert_eq!(
        w::role_member(&info(pc, rm.clone()), &pc, w::Role::Admin, &wk(7)),
        Err(w::DecodeError::Mismatch)
    );
    assert_eq!(
        w::role_member(&info(pc, rm.clone()), &pc, w::Role::Moderator, &wk(6)),
        Err(w::DecodeError::Mismatch)
    );
    assert_eq!(
        w::role_member(&info(ad, rm), &pc, w::Role::Moderator, &wk(7)),
        Err(w::DecodeError::WrongOwner)
    );
}

#[test]
fn checked_decoders_require_the_frozen_size() {
    use ad_pool_worker::rpc::AccountInfo;
    let ad = Pubkey::new_from_array(ad_pool::ID.to_bytes());
    let mut short = full_auction_bytes();
    short.truncate(700);
    let info = AccountInfo {
        lamports: 1,
        owner: ad,
        executable: false,
        data: short,
    };
    assert_eq!(
        w::ad_slot_auction(&info, &ad),
        Err(w::DecodeError::WrongSize {
            expected: 779,
            got: 700
        })
    );
}
