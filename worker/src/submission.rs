//! `POST /v1/creative` validation (plan W1), as pure functions so it is tested natively.
//! Two stages: `prevalidate` (no I/O: fields, image header, signature) runs before any RPC
//! read; `against_auction` (needs the decoded on-chain auction) runs after.

use solana_pubkey::Pubkey;

use crate::{
    accounts::{AD_CLICK_URL_MAX_LEN, AD_TEXT_MAX_LEN, AdSlotAuction, CreativeStatus},
    auth::{self, AuthError, SignedMessage},
    image::{self, ImageError, ImageInfo},
    pda, phase,
};

#[derive(Debug, Clone)]
pub struct Input {
    pub game: String,
    pub epoch: String,
    pub slot: String,
    pub signer: String,
    /// 32 lowercase hex chars, fresh per submission.
    pub nonce: String,
    /// unix seconds, (now, now + 600]
    pub expires: String,
    pub signature: String,
    pub image: Option<Vec<u8>>,
    /// Required for a full submission, must be absent for an image-only resubmission.
    pub text: Option<String>,
    pub click_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Kind {
    /// image + text + click_url — auction status must be `Empty`.
    Full,
    /// image only — auction status must be `TextOnly` (text/click already approved, A13).
    ImageOnly,
}

#[derive(Debug, Clone)]
pub struct Prepared {
    pub kind: Kind,
    pub game: Pubkey,
    pub epoch: u64,
    pub slot: u8,
    pub auction: Pubkey,
    pub signer: Pubkey,
    pub image: Vec<u8>,
    pub image_info: ImageInfo,
    pub image_sha256: [u8; 32],
    pub text: String,
    pub click_url: String,
    pub message: SignedMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Reject {
    BadField(&'static str),
    Image(ImageError),
    Auth(AuthError),
    /// decoded on-chain auction is not the (game, epoch, slot) that was signed
    AuctionMismatch,
    NotWinner,
    NotInCreativeWindow,
    AlreadyApproved,
    WrongKind {
        expected: Kind,
    },
}

impl Reject {
    pub fn status(&self) -> u16 {
        match self {
            Reject::BadField(_) | Reject::Image(_) => 400,
            // derived PDA holds a different auction — a chain/derivation anomaly, not the client
            Reject::AuctionMismatch => 502,
            Reject::Auth(_) => 401,
            Reject::NotWinner => 403,
            Reject::NotInCreativeWindow | Reject::AlreadyApproved | Reject::WrongKind { .. } => 409,
        }
    }
}

pub struct Limits<'a> {
    pub domain: &'a str,
    pub cluster: &'a str,
    pub ad_pool: &'a Pubkey,
    pub max_image_bytes: usize,
}

/// Invisible / direction-changing characters (Unicode Cf subset used for spoofing):
/// zero-width, LRM/RLM, bidi embeddings/overrides/isolates, word joiner, BOM.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}' | '\u{061C}' | '\u{180E}' | '\u{2028}' | '\u{2029}' | '\u{FFF9}'..='\u{FFFB}' | '\u{00AD}')
}

/// Same byte rule as the program's `approve_creative` (design §7.1) + no control or
/// invisible/bidi characters (the FE renders this as plain text next to a game).
fn check_text(t: &str) -> Result<(), Reject> {
    if t.is_empty()
        || t.len() > AD_TEXT_MAX_LEN
        || t.chars().any(|c| c.is_control() || is_invisible(c))
    {
        return Err(Reject::BadField("text"));
    }
    Ok(())
}

/// Program rule (lowercase https://, non-empty rest, no ASCII whitespace/control) — the
/// full layer-1 URL check (shorteners, IP hosts, punycode, redirects) is W2.
fn check_click_url(u: &str) -> Result<(), Reject> {
    if !u.starts_with("https://")
        || u.len() <= "https://".len()
        || u.len() > AD_CLICK_URL_MAX_LEN
        || u.bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(Reject::BadField("click_url"));
    }
    Ok(())
}

/// Stage 1 — no I/O: fields, image type / byte size / animation, payload hash, signature.
/// The slot's W×H is only known on chain, so the exact-size check runs in stage 2.
pub fn prevalidate(input: Input, limits: &Limits, now: i64) -> Result<Prepared, Reject> {
    let game: Pubkey = input.game.parse().map_err(|_| Reject::BadField("game"))?;
    let epoch: u64 = input.epoch.parse().map_err(|_| Reject::BadField("epoch"))?;
    let slot: u8 = input.slot.parse().map_err(|_| Reject::BadField("slot"))?;
    let signer: Pubkey = input
        .signer
        .parse()
        .map_err(|_| Reject::BadField("signer"))?;

    let kind = match (&input.text, &input.click_url) {
        (Some(_), Some(_)) => Kind::Full,
        (None, None) => Kind::ImageOnly,
        _ => return Err(Reject::BadField("text and click_url go together")),
    };
    let text = input.text.unwrap_or_default();
    let click_url = input.click_url.unwrap_or_default();
    if kind == Kind::Full {
        check_text(&text)?;
        check_click_url(&click_url)?;
    }

    let image = input.image.ok_or(Reject::BadField("image"))?;
    if image.len() > limits.max_image_bytes {
        return Err(Reject::Image(ImageError::TooLarge {
            max: limits.max_image_bytes,
            got: image.len(),
        }));
    }
    let image_info = image::sniff(&image).map_err(Reject::Image)?;
    let image_sha256 = auth::sha256(&image);

    let expires: i64 = input
        .expires
        .parse()
        .map_err(|_| Reject::BadField("expires"))?;
    // the Worker builds the message — every bound field comes from config or this request
    let message = SignedMessage {
        domain: limits.domain.to_string(),
        cluster: limits.cluster.to_string(),
        program: *limits.ad_pool,
        game,
        epoch,
        slot,
        payload: auth::payload_hash(&image_sha256, &text, &click_url),
        nonce: input.nonce,
        expires,
    };
    auth::verify(&message, &input.signature, &signer, now).map_err(Reject::Auth)?;

    Ok(Prepared {
        kind,
        game,
        epoch,
        slot,
        auction: pda::ad_slot(limits.ad_pool, &game, epoch, slot),
        signer,
        image,
        image_info,
        image_sha256,
        text,
        click_url,
        message,
    })
}

/// Stage 2 — against the decoded (owner-checked) on-chain auction at `p.auction`.
pub fn against_auction(p: &Prepared, a: &AdSlotAuction, now: i64) -> Result<(), Reject> {
    // the decoded account must be the one the signature names
    if a.game_program != p.game || a.epoch != p.epoch || a.slot_index != p.slot {
        return Err(Reject::AuctionMismatch);
    }
    if !phase::in_creative_window(now, a) {
        return Err(Reject::NotInCreativeWindow);
    }
    if a.highest_bidder != p.signer {
        return Err(Reject::NotWinner);
    }
    match (a.creative_status, p.kind) {
        (CreativeStatus::Approved, _) => return Err(Reject::AlreadyApproved),
        (CreativeStatus::Empty, Kind::ImageOnly) => {
            return Err(Reject::WrongKind {
                expected: Kind::Full,
            });
        }
        (CreativeStatus::TextOnly, Kind::Full) => {
            return Err(Reject::WrongKind {
                expected: Kind::ImageOnly,
            });
        }
        _ => {}
    }
    let (w, h) = (a.width as u32, a.height as u32);
    if (p.image_info.width, p.image_info.height) != (w, h) {
        return Err(Reject::Image(ImageError::WrongSize {
            expected: (w, h),
            got: (p.image_info.width, p.image_info.height),
        }));
    }
    Ok(())
}
