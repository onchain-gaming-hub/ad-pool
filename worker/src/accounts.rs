//! Read-only decoders for the on-chain accounts the Worker needs, by fixed offset.
//! Layouts are frozen by the program's golden tests (programs/ad_pool/tests/test_layout.rs)
//! and by platform-core's 01-account-layout-v1.md. The Worker never depends on Anchor in
//! Wasm; `tests/` cross-checks every decoder against bytes made by the real program types.

use solana_pubkey::Pubkey;

// Anchor discriminators = sha256("account:<Name>")[..8] (checked against the IDLs in tests).
pub const AD_SLOT_AUCTION_DISCRIMINATOR: [u8; 8] = [0x75, 0xd8, 0xcd, 0x3c, 0x31, 0xdf, 0xc4, 0x12];
pub const GAME_AD_CONFIG_DISCRIMINATOR: [u8; 8] = [0x33, 0xf0, 0xfd, 0xc4, 0x2c, 0x00, 0xa9, 0xd6];
pub const GAME_ENTRY_DISCRIMINATOR: [u8; 8] = [119, 212, 18, 192, 14, 188, 27, 149];
pub const PLATFORM_CONFIG_DISCRIMINATOR: [u8; 8] = [160, 78, 128, 0, 248, 83, 230, 160];
pub const ROLE_MEMBER_DISCRIMINATOR: [u8; 8] = [33, 119, 164, 224, 146, 251, 202, 109];

/// Layout version written in byte 8 of every account (ad-pool and platform-core).
pub const ACCOUNT_VERSION: u8 = 1;

// ad-pool sizes / offsets (= IDL constants of ad_pool)
pub const AD_SLOT_AUCTION_SIZE: usize = 779;
pub const GAME_AD_CONFIG_SIZE: usize = 199;
pub const CREATIVE_OFFSET: usize = 215;
pub const MAX_SLOTS: usize = 8;
pub const AD_IMAGE_URL_MAX_LEN: usize = 200;
pub const AD_TEXT_MAX_LEN: usize = 120;
pub const AD_CLICK_URL_MAX_LEN: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Account owner is not the expected program (never trust bytes alone).
    WrongOwner,
    /// Decoded content does not match what the address was derived for (e.g. role/member).
    Mismatch,
    /// Account size differs from the frozen layout.
    WrongSize {
        expected: usize,
        got: usize,
    },
    TooShort {
        need: usize,
        got: usize,
    },
    BadDiscriminator,
    UnsupportedVersion(u8),
    BadEnum(u8),
    BadString,
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn need(&self, end: usize) -> Result<(), DecodeError> {
        if self.data.len() < end {
            return Err(DecodeError::TooShort {
                need: end,
                got: self.data.len(),
            });
        }
        Ok(())
    }
    fn u8(&self, off: usize) -> Result<u8, DecodeError> {
        self.need(off + 1)?;
        Ok(self.data[off])
    }
    fn bool(&self, off: usize) -> Result<bool, DecodeError> {
        match self.u8(off)? {
            0 => Ok(false),
            1 => Ok(true),
            v => Err(DecodeError::BadEnum(v)),
        }
    }
    fn arr<const N: usize>(&self, off: usize) -> Result<[u8; N], DecodeError> {
        self.need(off + N)?;
        Ok(self.data[off..off + N].try_into().unwrap())
    }
    fn u16(&self, off: usize) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.arr(off)?))
    }
    fn u32(&self, off: usize) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.arr(off)?))
    }
    fn u64(&self, off: usize) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.arr(off)?))
    }
    fn i64(&self, off: usize) -> Result<i64, DecodeError> {
        Ok(i64::from_le_bytes(self.arr(off)?))
    }
    fn pubkey(&self, off: usize) -> Result<Pubkey, DecodeError> {
        Ok(Pubkey::new_from_array(self.arr(off)?))
    }
    /// Borsh string (u32 LE length + UTF-8 bytes) with a byte cap; returns (value, next offset).
    fn string(&self, off: usize, max: usize) -> Result<(String, usize), DecodeError> {
        let len = self.u32(off)? as usize;
        if len > max {
            return Err(DecodeError::BadString);
        }
        let start = off + 4;
        self.need(start + len)?;
        let s = core::str::from_utf8(&self.data[start..start + len])
            .map_err(|_| DecodeError::BadString)?;
        Ok((s.to_string(), start + len))
    }
}

fn head<'a>(data: &'a [u8], disc: &[u8; 8], min_len: usize) -> Result<Reader<'a>, DecodeError> {
    let r = Reader { data };
    r.need(min_len)?;
    if &data[..8] != disc {
        return Err(DecodeError::BadDiscriminator);
    }
    let v = data[8];
    if v != ACCOUNT_VERSION {
        return Err(DecodeError::UnsupportedVersion(v));
    }
    Ok(r)
}

// ------------------------------------------------------------------ ad-pool

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CreativeStatus {
    Empty,
    TextOnly,
    Approved,
}

impl CreativeStatus {
    fn from_u8(v: u8) -> Result<Self, DecodeError> {
        match v {
            0 => Ok(Self::Empty),
            1 => Ok(Self::TextOnly),
            2 => Ok(Self::Approved),
            v => Err(DecodeError::BadEnum(v)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct Creative {
    pub image_url: String,
    #[serde(with = "hex32")]
    pub image_sha256: [u8; 32],
    pub text: String,
    pub click_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AdSlotAuction {
    pub bump: u8,
    pub creative_status: CreativeStatus,
    pub settled: bool,
    pub slot_index: u8,
    #[serde(with = "b58")]
    pub game_program: Pubkey,
    pub epoch: u64,
    pub width: u16,
    pub height: u16,
    pub close_ts: i64,
    pub epoch_start_ts: i64,
    pub epoch_end_ts: i64,
    pub fee_bps: u16,
    pub min_increment_bps: u16,
    #[serde(with = "b58")]
    pub rent_payer: Pubkey,
    #[serde(with = "b58")]
    pub highest_bidder: Pubkey,
    pub highest_bid: u64,
    pub bid_count: u32,
    pub reject_count: u16,
    pub last_reject_flags: u32,
    pub approved_at: i64,
    pub updated_at: i64,
    pub creative: Creative,
}

pub fn decode_ad_slot_auction(data: &[u8]) -> Result<AdSlotAuction, DecodeError> {
    // smallest valid account = fixed head + empty creative (4 + 32 + 4 + 4)
    let r = head(data, &AD_SLOT_AUCTION_DISCRIMINATOR, CREATIVE_OFFSET + 44)?;
    let (image_url, next) = r.string(CREATIVE_OFFSET, AD_IMAGE_URL_MAX_LEN)?;
    let image_sha256 = r.arr::<32>(next)?;
    let (text, next) = r.string(next + 32, AD_TEXT_MAX_LEN)?;
    let (click_url, _) = r.string(next, AD_CLICK_URL_MAX_LEN)?;
    Ok(AdSlotAuction {
        bump: r.u8(9)?,
        creative_status: CreativeStatus::from_u8(r.u8(10)?)?,
        settled: r.bool(11)?,
        slot_index: r.u8(12)?,
        game_program: r.pubkey(13)?,
        epoch: r.u64(45)?,
        width: r.u16(53)?,
        height: r.u16(55)?,
        close_ts: r.i64(57)?,
        epoch_start_ts: r.i64(65)?,
        epoch_end_ts: r.i64(73)?,
        fee_bps: r.u16(81)?,
        min_increment_bps: r.u16(83)?,
        rent_payer: r.pubkey(85)?,
        highest_bidder: r.pubkey(117)?,
        highest_bid: r.u64(149)?,
        bid_count: r.u32(157)?,
        reject_count: r.u16(161)?,
        last_reject_flags: r.u32(163)?,
        approved_at: r.i64(167)?,
        updated_at: r.i64(175)?,
        creative: Creative {
            image_url,
            image_sha256,
            text,
            click_url,
        },
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct AdSlotSpec {
    pub width: u16,
    pub height: u16,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GameAdConfig {
    pub bump: u8,
    pub slot_count: u8,
    #[serde(with = "b58")]
    pub game_program: Pubkey,
    pub genesis_ts: i64,
    pub epoch_duration_secs: i64,
    pub auction_close_lead_secs: i64,
    pub min_bid: u64,
    pub min_increment_bps: u16,
    pub bid_horizon_epochs: u16,
    pub slots: [AdSlotSpec; MAX_SLOTS],
    pub created_at: i64,
    pub updated_at: i64,
}

pub fn decode_game_ad_config(data: &[u8]) -> Result<GameAdConfig, DecodeError> {
    let r = head(data, &GAME_AD_CONFIG_DISCRIMINATOR, GAME_AD_CONFIG_SIZE)?;
    let mut slots = [AdSlotSpec::default(); MAX_SLOTS];
    for (i, s) in slots.iter_mut().enumerate() {
        let off = 79 + i * 5;
        *s = AdSlotSpec {
            width: r.u16(off)?,
            height: r.u16(off + 2)?,
            active: r.bool(off + 4)?,
        };
    }
    Ok(GameAdConfig {
        bump: r.u8(9)?,
        slot_count: r.u8(10)?,
        game_program: r.pubkey(11)?,
        genesis_ts: r.i64(43)?,
        epoch_duration_secs: r.i64(51)?,
        auction_close_lead_secs: r.i64(59)?,
        min_bid: r.u64(67)?,
        min_increment_bps: r.u16(75)?,
        bid_horizon_epochs: r.u16(77)?,
        slots,
        created_at: r.i64(119)?,
        updated_at: r.i64(127)?,
    })
}

// ------------------------------------------------------------- platform-core

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum GameStatus {
    Active,
    Suspended,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GameEntry {
    pub status: GameStatus,
    #[serde(with = "b58")]
    pub game_program: Pubkey,
    #[serde(with = "b58")]
    pub owner: Pubkey,
}

/// pc 01 §4: status @10 · game_program @11 · owner @43 · 220 B.
pub fn decode_game_entry(data: &[u8]) -> Result<GameEntry, DecodeError> {
    let r = head(data, &GAME_ENTRY_DISCRIMINATOR, 220)?;
    let status = match r.u8(10)? {
        0 => GameStatus::Active,
        1 => GameStatus::Suspended,
        v => return Err(DecodeError::BadEnum(v)),
    };
    Ok(GameEntry {
        status,
        game_program: r.pubkey(11)?,
        owner: r.pubkey(43)?,
    })
}

/// platform-core `Role` — the byte is a PDA seed (pc 01 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[repr(u8)]
pub enum Role {
    Admin = 0,
    GameRegistrar = 1,
    Moderator = 2,
    FeeManager = 3,
    AllowlistManager = 4,
}

impl Role {
    fn from_u8(v: u8) -> Result<Self, DecodeError> {
        Ok(match v {
            0 => Self::Admin,
            1 => Self::GameRegistrar,
            2 => Self::Moderator,
            3 => Self::FeeManager,
            4 => Self::AllowlistManager,
            v => return Err(DecodeError::BadEnum(v)),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RoleMember {
    pub role: Role,
    #[serde(with = "b58")]
    pub member: Pubkey,
}

/// pc 01 §3: role @10 · member @11 · 115 B. Existence of the PDA = has the role.
pub fn decode_role_member(data: &[u8]) -> Result<RoleMember, DecodeError> {
    let r = head(data, &ROLE_MEMBER_DISCRIMINATOR, 115)?;
    Ok(RoleMember {
        role: Role::from_u8(r.u8(10)?)?,
        member: r.pubkey(11)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PlatformConfig {
    pub boost_fee_bps: u16,
    pub ad_fee_bps: u16,
    pub treasury_bump: u8,
}

/// pc 01 §2: boost_fee_bps @10 · ad_fee_bps @12 · admin_count @14 · treasury_bump @16 · 97 B.
pub fn decode_platform_config(data: &[u8]) -> Result<PlatformConfig, DecodeError> {
    let r = head(data, &PLATFORM_CONFIG_DISCRIMINATOR, 97)?;
    Ok(PlatformConfig {
        boost_fee_bps: r.u16(10)?,
        ad_fee_bps: r.u16(12)?,
        treasury_bump: r.u8(16)?,
    })
}

// ------------------------------------------------------- owner-checked entry points
// Routes must use these: a decoder alone would accept look-alike bytes owned by anyone.

use crate::rpc::AccountInfo;

/// On-chain accounts have frozen sizes (pc 01 §0 · design §2/§3) — a different size is not ours.
fn exact(info: &AccountInfo, expected: usize) -> Result<(), DecodeError> {
    if info.data.len() != expected {
        return Err(DecodeError::WrongSize {
            expected,
            got: info.data.len(),
        });
    }
    Ok(())
}

fn owned_by(info: &AccountInfo, program: &Pubkey) -> Result<(), DecodeError> {
    if info.owner != *program {
        return Err(DecodeError::WrongOwner);
    }
    Ok(())
}

pub fn ad_slot_auction(info: &AccountInfo, ad_pool: &Pubkey) -> Result<AdSlotAuction, DecodeError> {
    owned_by(info, ad_pool)?;
    exact(info, AD_SLOT_AUCTION_SIZE)?;
    decode_ad_slot_auction(&info.data)
}

pub fn game_ad_config(info: &AccountInfo, ad_pool: &Pubkey) -> Result<GameAdConfig, DecodeError> {
    owned_by(info, ad_pool)?;
    exact(info, GAME_AD_CONFIG_SIZE)?;
    decode_game_ad_config(&info.data)
}

pub fn game_entry(info: &AccountInfo, platform_core: &Pubkey) -> Result<GameEntry, DecodeError> {
    owned_by(info, platform_core)?;
    exact(info, 220)?;
    decode_game_entry(&info.data)
}

/// Also checks the decoded role/member against what the PDA was derived for.
pub fn role_member(
    info: &AccountInfo,
    platform_core: &Pubkey,
    role: Role,
    member: &Pubkey,
) -> Result<RoleMember, DecodeError> {
    owned_by(info, platform_core)?;
    exact(info, 115)?;
    let rm = decode_role_member(&info.data)?;
    if rm.role != role || rm.member != *member {
        return Err(DecodeError::Mismatch);
    }
    Ok(rm)
}

pub fn platform_config(
    info: &AccountInfo,
    platform_core: &Pubkey,
) -> Result<PlatformConfig, DecodeError> {
    owned_by(info, platform_core)?;
    exact(info, 97)?;
    decode_platform_config(&info.data)
}

// ------------------------------------------------------------- serde helpers

mod b58 {
    pub fn serialize<S: serde::Serializer>(
        k: &solana_pubkey::Pubkey,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_str(&k.to_string())
    }
}

mod hex32 {
    pub fn serialize<S: serde::Serializer>(b: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        let mut out = String::with_capacity(64);
        for x in b {
            out.push_str(&format!("{x:02x}"));
        }
        s.serialize_str(&out)
    }
}
