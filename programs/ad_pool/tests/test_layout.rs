//! Golden layout tests (P0) — sizes, offsets, discriminators and enum bytes must match
//! specs/designs/01-program-design-v1.md §2/§3. A failure here means the FE, the Worker
//! or a memcmp filter would read the account wrong.

use {
    ad_pool::{
        constants::*,
        math::AD_SLOT_AUCTION_SPACE,
        platform_core,
        state::{AdCreative, AdSlotAuction, AdSlotSpec, CreativeStatus, GameAdConfig},
    },
    anchor_lang::{prelude::Pubkey, AccountSerialize, AnchorSerialize, Discriminator, Space},
    litesvm::LiteSVM,
    sha2::{Digest, Sha256},
};

const DISC: usize = 8;

fn serialize<T: AccountSerialize>(acct: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    acct.try_serialize(&mut buf).unwrap();
    buf
}

fn to_bytes<T: AnchorSerialize>(value: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    value.serialize(&mut buf).unwrap();
    buf
}

fn pk(byte: u8) -> Pubkey {
    Pubkey::new_from_array([byte; 32])
}

fn expected_discriminator(name: &str) -> [u8; 8] {
    let hash = Sha256::digest(format!("account:{name}").as_bytes());
    hash[..8].try_into().unwrap()
}

fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(b[off..off + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}
fn i64_at(b: &[u8], off: usize) -> i64 {
    i64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}
fn pk_at(b: &[u8], off: usize) -> Pubkey {
    Pubkey::new_from_array(b[off..off + 32].try_into().unwrap())
}

fn full_creative() -> AdCreative {
    AdCreative {
        image_url: "u".repeat(AD_IMAGE_URL_MAX_LEN as usize),
        image_sha256: [0xAB; 32],
        text: "t".repeat(AD_TEXT_MAX_LEN as usize),
        click_url: "c".repeat(AD_CLICK_URL_MAX_LEN as usize),
    }
}

fn sample_config() -> GameAdConfig {
    let mut slots = [AdSlotSpec::default(); MAX_SLOTS as usize];
    slots[0] = AdSlotSpec {
        width: 728,
        height: 90,
        active: true,
    };
    slots[7] = AdSlotSpec {
        width: 0x0807,
        height: 0x0908,
        active: true,
    };
    GameAdConfig {
        version: ACCOUNT_VERSION,
        bump: 254,
        slot_count: 8,
        game_program: pk(0x11),
        genesis_ts: 0x0102_0304_0506_0708,
        epoch_duration_secs: 86_400,
        auction_close_lead_secs: 7 * 86_400,
        min_bid: 0x1122_3344_5566_7788,
        min_increment_bps: 0xA1A2,
        bid_horizon_epochs: 0xB1B2,
        slots,
        created_at: 0x2122_2324_2526_2728,
        updated_at: 0x3132_3334_3536_3738,
        _reserved: [0; 64],
    }
}

fn sample_auction(creative: AdCreative) -> AdSlotAuction {
    AdSlotAuction {
        version: ACCOUNT_VERSION,
        bump: 253,
        creative_status: CreativeStatus::Approved,
        settled: true,
        slot_index: 7,
        game_program: pk(0x22),
        epoch: 0x0102_0304_0506_0708,
        width: 0x1112,
        height: 0x1314,
        close_ts: 0x2122_2324_2526_2728,
        epoch_start_ts: 0x3132_3334_3536_3738,
        epoch_end_ts: 0x4142_4344_4546_4748,
        fee_bps: 0x5152,
        min_increment_bps: 0x5354,
        rent_payer: pk(0x33),
        highest_bidder: pk(0x44),
        highest_bid: 0x6162_6364_6566_6768,
        bid_count: 0x7172_7374,
        reject_count: 0x8182,
        last_reject_flags: 0x9192_9394,
        approved_at: 0x0A0B_0C0D_0E0F_1011,
        updated_at: 0x1A1B_1C1D_1E1F_2021,
        _reserved: [0; 32],
        creative,
    }
}

// ---------------------------------------------------------------- sizes

#[test]
fn account_sizes_match_design() {
    assert_eq!(DISC + GameAdConfig::INIT_SPACE, 199, "GameAdConfig");
    assert_eq!(
        DISC + AdSlotAuction::INIT_SPACE,
        779,
        "AdSlotAuction (fully reserved)"
    );
    assert_eq!(AD_SLOT_AUCTION_SPACE, 779);
    assert_eq!(
        AdCreative::INIT_SPACE,
        564,
        "AdCreative = (4+200) + 32 + (4+120) + (4+200)"
    );
    assert_eq!(AdSlotSpec::INIT_SPACE, 5);
    assert_eq!(
        GAME_AD_CONFIG_SIZE as usize,
        DISC + GameAdConfig::INIT_SPACE
    );
    assert_eq!(
        AD_SLOT_AUCTION_SIZE as usize,
        DISC + AdSlotAuction::INIT_SPACE
    );
    assert_eq!(CreativeStatus::INIT_SPACE, 1);
}

#[test]
fn full_accounts_serialize_to_reserved_size() {
    // Max-length strings must fill the reserved space exactly (no overflow on write).
    assert_eq!(serialize(&sample_config()).len(), 199);
    assert_eq!(serialize(&sample_auction(full_creative())).len(), 779);
}

// ---------------------------------------------------------- discriminators

#[test]
fn discriminators_are_anchor_default() {
    assert_eq!(
        GameAdConfig::DISCRIMINATOR,
        expected_discriminator("GameAdConfig")
    );
    assert_eq!(
        AdSlotAuction::DISCRIMINATOR,
        expected_discriminator("AdSlotAuction")
    );
    let b = serialize(&sample_config());
    assert_eq!(&b[..8], &expected_discriminator("GameAdConfig"));
}

// ---------------------------------------------------------------- offsets

#[test]
fn game_ad_config_offsets() {
    let c = sample_config();
    let b = serialize(&c);
    assert_eq!(b[8], ACCOUNT_VERSION, "version @8");
    assert_eq!(b[9], 254, "bump @9");
    assert_eq!(b[10], 8, "slot_count @10");
    assert_eq!(pk_at(&b, 11), c.game_program, "game_program @11");
    assert_eq!(i64_at(&b, 43), c.genesis_ts, "genesis_ts @43");
    assert_eq!(
        i64_at(&b, 51),
        c.epoch_duration_secs,
        "epoch_duration_secs @51"
    );
    assert_eq!(
        i64_at(&b, 59),
        c.auction_close_lead_secs,
        "auction_close_lead_secs @59"
    );
    assert_eq!(u64_at(&b, 67), c.min_bid, "min_bid @67");
    assert_eq!(u16_at(&b, 75), c.min_increment_bps, "min_increment_bps @75");
    assert_eq!(
        u16_at(&b, 77),
        c.bid_horizon_epochs,
        "bid_horizon_epochs @77"
    );
    // slots @79, 5 bytes each: width u16, height u16, active bool
    assert_eq!(u16_at(&b, 79), 728);
    assert_eq!(u16_at(&b, 81), 90);
    assert_eq!(b[83], 1);
    assert_eq!(u16_at(&b, 79 + 7 * 5), 0x0807, "slots[7].width @114");
    assert_eq!(u16_at(&b, 81 + 7 * 5), 0x0908);
    assert_eq!(b[83 + 7 * 5], 1, "slots[7].active @118");
    assert_eq!(i64_at(&b, 119), c.created_at, "created_at @119");
    assert_eq!(i64_at(&b, 127), c.updated_at, "updated_at @127");
    assert!(b[135..199].iter().all(|x| *x == 0), "_reserved @135..199");
}

#[test]
fn ad_slot_auction_offsets() {
    let a = sample_auction(full_creative());
    let b = serialize(&a);
    assert_eq!(b[8], ACCOUNT_VERSION, "version @8");
    assert_eq!(b[9], 253, "bump @9");
    assert_eq!(
        b[10],
        CreativeStatus::Approved as u8,
        "creative_status @10 (memcmp)"
    );
    assert_eq!(b[11], 1, "settled @11 (memcmp)");
    assert_eq!(b[12], 7, "slot_index @12");
    assert_eq!(pk_at(&b, 13), a.game_program, "game_program @13");
    assert_eq!(u64_at(&b, 45), a.epoch, "epoch @45");
    assert_eq!(u16_at(&b, 53), a.width, "width @53");
    assert_eq!(u16_at(&b, 55), a.height, "height @55");
    assert_eq!(i64_at(&b, 57), a.close_ts, "close_ts @57");
    assert_eq!(i64_at(&b, 65), a.epoch_start_ts, "epoch_start_ts @65");
    assert_eq!(i64_at(&b, 73), a.epoch_end_ts, "epoch_end_ts @73");
    assert_eq!(u16_at(&b, 81), a.fee_bps, "fee_bps @81");
    assert_eq!(u16_at(&b, 83), a.min_increment_bps, "min_increment_bps @83");
    assert_eq!(pk_at(&b, 85), a.rent_payer, "rent_payer @85 (memcmp)");
    assert_eq!(pk_at(&b, 117), a.highest_bidder, "highest_bidder @117");
    assert_eq!(u64_at(&b, 149), a.highest_bid, "highest_bid @149");
    assert_eq!(u32_at(&b, 157), a.bid_count, "bid_count @157");
    assert_eq!(u16_at(&b, 161), a.reject_count, "reject_count @161");
    assert_eq!(
        u32_at(&b, 163),
        a.last_reject_flags,
        "last_reject_flags @163"
    );
    assert_eq!(i64_at(&b, 167), a.approved_at, "approved_at @167");
    assert_eq!(i64_at(&b, 175), a.updated_at, "updated_at @175");
    assert!(b[183..215].iter().all(|x| *x == 0), "_reserved @183..215");
    // creative @215: image_url (u32 len + bytes) first
    assert_eq!(
        u32_at(&b, 215),
        AD_IMAGE_URL_MAX_LEN as u32,
        "creative.image_url len @215"
    );
    assert_eq!(&b[215..], &to_bytes(&a.creative)[..], "creative = tail");
    // exported memcmp constants point at the same bytes
    assert_eq!(
        b[CREATIVE_STATUS_OFFSET as usize],
        CreativeStatus::Approved as u8
    );
    assert_eq!(b[SETTLED_OFFSET as usize], 1);
    let rp = RENT_PAYER_OFFSET as usize;
    assert_eq!(pk_at(&b, rp), a.rent_payer);
    assert_eq!(CREATIVE_OFFSET, 215);
}

#[test]
fn fixed_offsets_do_not_move_with_short_creative() {
    // Variable-length strings sit last, so a short creative never shifts a fixed field.
    let short = sample_auction(AdCreative::default());
    let full = sample_auction(full_creative());
    assert_eq!(serialize(&short)[..215], serialize(&full)[..215]);
}

// ---------------------------------------------------------------- enums

#[test]
fn creative_status_bytes_frozen_v1() {
    assert_eq!(to_bytes(&CreativeStatus::Empty), vec![0]);
    assert_eq!(to_bytes(&CreativeStatus::TextOnly), vec![1]);
    assert_eq!(to_bytes(&CreativeStatus::Approved), vec![2]);
}

#[test]
fn creative_status_byte_equals_declaration_index() {
    // The IDL has no discriminant values: IDL consumers encode the variant INDEX.
    for (index, v) in [
        CreativeStatus::Empty,
        CreativeStatus::TextOnly,
        CreativeStatus::Approved,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(v as u8 as usize, index);
    }
}

/// Compile-time tripwire: adding a variant breaks this match → update the frozen tests.
#[allow(dead_code)]
fn enum_tripwire(s: CreativeStatus) -> u8 {
    match s {
        CreativeStatus::Empty => 0,
        CreativeStatus::TextOnly => 1,
        CreativeStatus::Approved => 2,
    }
}

// ------------------------------------------------ platform-core compatibility

#[test]
fn ad_creative_bytes_equal_platform_core_type() {
    // FE uses ONE renderer for AdSlotAuction.creative and pc DefaultAd.creative.
    let ours = full_creative();
    let theirs = platform_core::types::AdCreative {
        image_url: ours.image_url.clone(),
        image_sha256: ours.image_sha256,
        text: ours.text.clone(),
        click_url: ours.click_url.clone(),
    };
    assert_eq!(to_bytes(&ours), to_bytes(&theirs));
}

#[test]
fn shared_constants_equal_platform_core_idl() {
    let path = format!(
        "{}/../../idls/platform_core.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let idl: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let get = |name: &str| -> String {
        idl["constants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("{name} not in platform_core IDL"))["value"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let bytes = |seed: &[u8]| format!("{seed:?}");
    let ty = |name: &str| -> String {
        idl["constants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap()["type"]
            .as_str()
            .unwrap_or("?")
            .to_string()
    };
    // same TYPE too — a merged SDK must not see two types for one constant
    for name in [
        "AD_IMAGE_URL_MAX_LEN",
        "AD_TEXT_MAX_LEN",
        "AD_CLICK_URL_MAX_LEN",
    ] {
        assert_eq!(ty(name), "u16", "{name} type in pc IDL");
    }
    assert_eq!(
        get("AD_IMAGE_URL_MAX_LEN"),
        AD_IMAGE_URL_MAX_LEN.to_string()
    );
    assert_eq!(get("AD_TEXT_MAX_LEN"), AD_TEXT_MAX_LEN.to_string());
    assert_eq!(
        get("AD_CLICK_URL_MAX_LEN"),
        AD_CLICK_URL_MAX_LEN.to_string()
    );
    assert_eq!(get("MAX_FEE_BPS"), MAX_FEE_BPS.to_string());
    assert_eq!(get("BPS_DENOMINATOR"), BPS_DENOMINATOR.to_string());
    assert_eq!(get("CONFIG_SEED"), bytes(PC_CONFIG_SEED));
    assert_eq!(get("TREASURY_SEED"), bytes(PC_TREASURY_SEED));
    assert_eq!(get("ROLE_SEED"), bytes(PC_ROLE_SEED));
    assert_eq!(get("GAME_SEED"), bytes(PC_GAME_SEED));
}

#[test]
fn platform_core_program_id_is_devnet_deployment() {
    assert_eq!(
        platform_core::ID.to_string(),
        "AJnPSYsn7J2muHfKztRtpZz5kBP94dWkNZLr9aMhu18G"
    );
}

// ---------------------------------------------------------------- runtime

#[test]
fn litesvm_loads_program_built_with_cargo_build_sbf_4_3() {
    // Reads the PRE-BUILT .so at runtime (run ./scripts/test.sh, which builds first).
    let path = format!("{}/../deploy/ad_pool.so", env!("CARGO_TARGET_TMPDIR"));
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("{path}: {e} — build first: ./scripts/test.sh"));
    let mut svm = LiteSVM::new();
    svm.add_program(ad_pool::id(), &bytes).unwrap();
}

#[test]
fn constants_match_decisions() {
    assert_eq!(MIN_CLOSE_LEAD_SECS, 7 * 86_400, "D5");
    assert_eq!(MAX_EPOCH_DURATION_SECS, 30 * 86_400, "Q11");
    assert_eq!(MAX_HORIZON_SECS, 90 * 86_400, "Q11");
    assert_eq!(MAX_SLOTS, 8);
    assert_eq!(AD_CONFIG_SEED, b"ad_config");
    assert_eq!(AD_SLOT_SEED, b"ad_slot");
}

// ---------------------------------------------------------------- errors

#[test]
fn error_codes_frozen_v1() {
    // Codes are interface: FE retries on LeaderChanged / BidTooLow. Append only.
    use ad_pool::error::AdPoolError as E;
    let code = |e: E| u32::from(e); // Anchor's real mapping (adds ERROR_CODE_OFFSET)
    assert_eq!(code(E::GameNotActive), 6000);
    assert_eq!(code(E::FeeTooHigh), 6007);
    assert_eq!(code(E::BidTooLow), 6016);
    assert_eq!(code(E::LeaderChanged), 6017);
    assert_eq!(code(E::NotInCreativeWindow), 6018);
    assert_eq!(code(E::NotSettled), 6029);
    assert_eq!(code(E::RentPayerMismatch), 6030);
    assert_eq!(code(E::MathOverflow), 6031);
}

#[test]
fn reject_flags_are_distinct_bits() {
    let flags = [
        REJECT_FILE_TYPE,
        REJECT_DIMENSIONS,
        REJECT_IMAGE_MODERATION,
        REJECT_TEXT_MODERATION,
        REJECT_URL_RULES,
        REJECT_URL_REPUTATION,
        REJECT_TOO_LONG,
    ];
    for (i, f) in flags.iter().enumerate() {
        assert_eq!(*f, 1 << i);
    }
}
