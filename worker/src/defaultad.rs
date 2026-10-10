//! W4 `POST /v1/default-ad` (platform-core Q3 · pc Q-A..D): a platform-core **Admin** submits
//! the default ad for one slot size. It goes through the same pipeline as a winner's creative
//! (image structure + exact W×H, text, click_url layers 1–2, OpenAI image + text) — no
//! shortcut — then the Worker signs pc `create_default_ad` / `update_default_ad` under its
//! own **Moderator** role. No endpoint for `set_default_ad_enabled` (the Admin signs that
//! from their own wallet).
//! Pure (tested natively); the route is in lib.rs.

use solana_pubkey::Pubkey;

use crate::{
    auth::{self, AuthError, DefaultAdMessage},
    decision::Check,
    image::{self, ImageError, ImageInfo},
    submission::{canonical, check_click_url, check_text},
};

#[derive(Debug, Clone)]
pub struct Input {
    pub width: String,
    pub height: String,
    /// the Admin wallet that signed
    pub signer: String,
    pub nonce: String,
    pub expires: String,
    pub signature: String,
    pub image: Option<Vec<u8>>,
    pub text: Option<String>,
    pub click_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Prepared {
    pub width: u16,
    pub height: u16,
    pub signer: Pubkey,
    pub image: Vec<u8>,
    pub image_info: ImageInfo,
    pub image_sha256: [u8; 32],
    pub text: String,
    pub click_url: String,
    pub click_url_normalized: String,
    pub message: DefaultAdMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Reject {
    BadField(&'static str),
    ClickUrl(crate::urlcheck::UrlReject),
    Image(ImageError),
    Auth(AuthError),
    /// signer has no `RoleMember(Admin)` on platform-core (Moderator does not count — pc Q-A)
    NotAdmin,
}

impl Reject {
    pub fn status(&self) -> u16 {
        match self {
            Reject::BadField(_) | Reject::ClickUrl(_) | Reject::Image(_) => 400,
            Reject::Auth(_) => 401,
            Reject::NotAdmin => 403,
        }
    }
}

impl From<crate::submission::Reject> for Reject {
    fn from(r: crate::submission::Reject) -> Self {
        use crate::submission::Reject as S;
        match r {
            S::BadField(f) => Reject::BadField(f),
            S::ClickUrl(u) => Reject::ClickUrl(u),
            S::Image(i) => Reject::Image(i),
            S::Auth(a) => Reject::Auth(a),
            // chain-stage rejects: never produced by the shared local validators — listed
            // explicitly so a new variant forces a decision here
            S::AuctionMismatch
            | S::NotWinner
            | S::NotInCreativeWindow
            | S::AlreadyApproved
            | S::WrongKind { .. } => Reject::BadField("internal"),
        }
    }
}

pub struct Limits<'a> {
    pub domain: &'a str,
    pub cluster: &'a str,
    pub platform_core: &'a Pubkey,
    pub max_image_bytes: usize,
}

/// No I/O: fields, image (type, structure, size, **exact W×H of the requested size**), text,
/// click_url layer 1, signature over the Worker-built message.
pub fn prevalidate(input: Input, limits: &Limits, now: i64) -> Result<Prepared, Reject> {
    let width: u16 = canonical(&input.width).ok_or(Reject::BadField("width"))?;
    let height: u16 = canonical(&input.height).ok_or(Reject::BadField("height"))?;
    if width == 0 || height == 0 {
        return Err(Reject::BadField("width/height"));
    }
    let signer: Pubkey = input
        .signer
        .parse()
        .map_err(|_| Reject::BadField("signer"))?;
    // a default ad is always complete: pc requires image_sha256 != 0 and https click_url
    let text = input.text.ok_or(Reject::BadField("text"))?;
    let click_url = input.click_url.ok_or(Reject::BadField("click_url"))?;
    check_text(&text)?;
    let click_url_normalized = check_click_url(&click_url)?;

    let img = input.image.ok_or(Reject::BadField("image"))?;
    let image_info =
        image::check(&img, width, height, limits.max_image_bytes).map_err(Reject::Image)?;
    let image_sha256 = auth::sha256(&img);

    let expires: i64 = input
        .expires
        .parse()
        .map_err(|_| Reject::BadField("expires"))?;
    let message = DefaultAdMessage {
        domain: limits.domain.to_string(),
        cluster: limits.cluster.to_string(),
        program: *limits.platform_core,
        width,
        height,
        payload: auth::payload_hash(&image_sha256, &text, &click_url),
        nonce: input.nonce,
        expires,
    };
    auth::verify_rendered(
        &message.render(),
        &message.nonce,
        message.expires,
        &input.signature,
        &signer,
        now,
    )
    .map_err(Reject::Auth)?;
    Ok(Prepared {
        width,
        height,
        signer,
        image: img,
        image_info,
        image_sha256,
        text,
        click_url,
        click_url_normalized,
        message,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Outcome {
    /// every check passed → create or update the DefaultAd
    Publish,
    /// at least one check failed — nothing goes on chain (pc has no reject record for
    /// default ads); the Admin gets the reasons
    Reject { flags: u32, reasons: Vec<String> },
    /// a check could not run and nothing failed — retry later
    Retry { unavailable: Vec<String> },
}

/// Stricter than §7.5: no text-only fallback (a DefaultAd must have an image).
pub fn decide(image: &Check, text: &Check, url: &Check) -> Outcome {
    let all = [image, text, url];
    let mut flags = 0;
    let mut reasons = vec![];
    let mut unavailable = vec![];
    for c in all {
        match c {
            Check::Pass => {}
            Check::Fail { flags: f, detail } => {
                flags |= f.get();
                reasons.extend(detail.iter().cloned());
            }
            Check::Unavailable(r) => unavailable.push(r.clone()),
        }
    }
    if flags != 0 {
        Outcome::Reject { flags, reasons }
    } else if !unavailable.is_empty() {
        Outcome::Retry { unavailable }
    } else {
        Outcome::Publish
    }
}

/// Per-size gate key (shares the `AuctionGate` DO class: nonce + rate limit + in-flight lock).
pub fn gate_name(width: u16, height: u16) -> String {
    format!("default_ad:{width}x{height}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::flags::*;

    #[test]
    fn decide_has_no_text_only_and_fails_closed() {
        let p = Check::Pass;
        let img = Check::fail(IMAGE_MODERATION, "image:sexual");
        let url = Check::fail(URL_LAYER2, "web_risk:MALWARE");
        let down = Check::Unavailable("openai_text:timeout".into());
        assert_eq!(decide(&p, &p, &p), Outcome::Publish);
        assert_eq!(
            decide(&img, &p, &p),
            Outcome::Reject {
                flags: IMAGE_MODERATION,
                reasons: vec!["image:sexual".into()]
            }
        );
        assert_eq!(
            decide(&img, &down, &url),
            Outcome::Reject {
                flags: IMAGE_MODERATION | URL_LAYER2,
                reasons: vec!["image:sexual".into(), "web_risk:MALWARE".into()]
            }
        );
        assert_eq!(
            decide(&p, &down, &p),
            Outcome::Retry {
                unavailable: vec!["openai_text:timeout".into()]
            }
        );
    }

    #[test]
    fn gate_names_do_not_collide_with_auction_addresses() {
        assert_eq!(gate_name(728, 90), "default_ad:728x90");
        // auction gates are named by base58 addresses, which never contain ':'
        assert!(gate_name(1, 1).contains(':'));
    }
}
