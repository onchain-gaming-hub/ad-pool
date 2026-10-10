//! W1 — creative intake: image structure, wallet-signature auth (Worker-built message),
//! submission validation, per-auction gate. Native; the Wasm route only wires these.

use {
    ad_pool_worker::{
        accounts::{AdSlotAuction, Creative, CreativeStatus},
        auth::{self, AuthError, SignedMessage},
        gate::{self, GateError, GateState},
        image::{self, ImageError, ImageKind},
        pda,
        submission::{self, Input, Kind, Limits, Reject},
    },
    solana_keypair::Keypair,
    solana_pubkey::Pubkey,
    solana_signer::Signer,
};

const NOW: i64 = 1_790_000_000;
const DOMAIN: &str = "ad-pool.devnet.local";
const NONCE: &str = "0123456789abcdef0123456789abcdef";

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn ad_pool_id() -> Pubkey {
    "6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk"
        .parse()
        .unwrap()
}

// ------------------------------------------------------------- image builders

fn riff(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut body = b"WEBP".to_vec();
    for (k, d) in chunks {
        body.extend_from_slice(*k);
        body.extend_from_slice(&(d.len() as u32).to_le_bytes());
        body.extend_from_slice(d);
        if d.len() % 2 == 1 {
            body.push(0);
        }
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}
fn vp8l(w: u32, h: u32) -> Vec<u8> {
    let mut d = vec![0x2F];
    d.extend_from_slice(&((w - 1) | ((h - 1) << 14)).to_le_bytes());
    d
}
fn vp8(w: u16, h: u16) -> Vec<u8> {
    let mut d = vec![0, 0, 0, 0x9d, 0x01, 0x2a];
    d.extend_from_slice(&w.to_le_bytes());
    d.extend_from_slice(&h.to_le_bytes());
    d
}
fn vp8x(w: u32, h: u32, flags: u8) -> Vec<u8> {
    let mut d = vec![flags, 0, 0, 0];
    d.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
    d.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
    d
}

/// Minimal big-endian TIFF with IFD0 entries (tag, value).
fn tiff(entries: &[(u16, u16)]) -> Vec<u8> {
    let mut t = b"MM\0*".to_vec();
    t.extend_from_slice(&8u32.to_be_bytes());
    t.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    for (tag, v) in entries {
        t.extend_from_slice(&tag.to_be_bytes());
        t.extend_from_slice(&3u16.to_be_bytes()); // SHORT
        t.extend_from_slice(&1u32.to_be_bytes());
        t.extend_from_slice(&v.to_be_bytes());
        t.extend_from_slice(&[0, 0]);
    }
    t.extend_from_slice(&0u32.to_be_bytes());
    t
}

/// Insert an APP1 Exif segment right after SOI of the real JPEG.
fn jpeg_with_exif(t: Vec<u8>) -> Vec<u8> {
    let j = fixture("b728x90.jpg");
    let mut seg = b"Exif\0\0".to_vec();
    seg.extend(t);
    let mut out = j[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&((seg.len() + 2) as u16).to_be_bytes());
    out.extend(seg);
    out.extend_from_slice(&j[2..]);
    out
}

/// Insert a chunk right after IHDR of the real PNG.
fn png_with_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let png = fixture("b728x90.png");
    let mut out = png[..33].to_vec(); // sig(8) + IHDR chunk(25)
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&[0; 4]); // CRC (not verified)
    out.extend_from_slice(&png[33..]);
    out
}

// ------------------------------------------------------------------ image

#[test]
fn accepts_real_png_and_jpeg_and_valid_webp() {
    let png = image::sniff(&fixture("b728x90.png")).unwrap();
    assert_eq!((png.kind, png.width, png.height), (ImageKind::Png, 728, 90));
    // sips adds a harmless EXIF block (no orientation, no GPS) — must be accepted
    let jpg = image::sniff(&fixture("b728x90.jpg")).unwrap();
    assert_eq!(
        (jpg.kind, jpg.width, jpg.height),
        (ImageKind::Jpeg, 728, 90)
    );
    for (bytes, w, h) in [
        (riff(&[(b"VP8L", vp8l(300, 250))]), 300, 250),
        (riff(&[(b"VP8 ", vp8(160, 600))]), 160, 600),
        (
            riff(&[(b"VP8X", vp8x(728, 90, 0)), (b"VP8L", vp8l(728, 90))]),
            728,
            90,
        ),
    ] {
        let i = image::sniff(&bytes).unwrap();
        assert_eq!((i.kind, i.width, i.height), (ImageKind::Webp, w, h));
    }
}

#[test]
fn rejects_animation_in_every_format() {
    assert_eq!(
        image::sniff(&png_with_chunk(b"acTL", &[0; 8])),
        Err(ImageError::Animated)
    );
    assert_eq!(
        image::sniff(&riff(&[
            (b"VP8X", vp8x(728, 90, 0x02)),
            (b"VP8L", vp8l(728, 90))
        ])),
        Err(ImageError::Animated)
    );
    // animation flag cleared but ANMF frames present
    assert_eq!(
        image::sniff(&riff(&[
            (b"VP8X", vp8x(728, 90, 0)),
            (b"ANMF", vec![0; 16]),
            (b"VP8L", vp8l(728, 90))
        ])),
        Err(ImageError::Animated)
    );
}

#[test]
fn exif_orientation_and_gps_are_rejected_harmless_exif_is_fine() {
    assert!(image::sniff(&jpeg_with_exif(tiff(&[(0x0112, 1)]))).is_ok());
    assert_eq!(
        image::sniff(&jpeg_with_exif(tiff(&[(0x0112, 6)]))),
        Err(ImageError::Exif)
    );
    assert_eq!(
        image::sniff(&jpeg_with_exif(tiff(&[(0x8825, 0)]))),
        Err(ImageError::Exif)
    );
    assert_eq!(
        image::sniff(&jpeg_with_exif(b"garbage!".to_vec())),
        Err(ImageError::Exif)
    );
    assert_eq!(
        image::sniff(&png_with_chunk(b"eXIf", &tiff(&[(0x0112, 8)]))),
        Err(ImageError::Exif)
    );
    assert!(image::sniff(&png_with_chunk(b"eXIf", &tiff(&[(0x0112, 1)]))).is_ok());
    assert_eq!(
        image::sniff(&riff(&[
            (b"VP8X", vp8x(728, 90, 0x08)),
            (b"EXIF", tiff(&[(0x0112, 3)])),
            (b"VP8L", vp8l(728, 90))
        ])),
        Err(ImageError::Exif)
    );
}

#[test]
fn rejects_polyglots_and_broken_structure() {
    let png = fixture("b728x90.png");
    // header-only PNG followed by HTML
    let mut poly = png[..33].to_vec();
    poly.extend_from_slice(b"<html><script>alert(1)</script>");
    assert!(matches!(image::sniff(&poly), Err(ImageError::Malformed(_))));
    // trailing bytes after IEND
    let mut trail = png.clone();
    trail.extend_from_slice(b"<script>");
    assert_eq!(
        image::sniff(&trail),
        Err(ImageError::Malformed("png trailing bytes"))
    );
    // IHDR length != 13
    let mut ihdr = png.clone();
    ihdr[8..12].copy_from_slice(&0u32.to_be_bytes());
    assert_eq!(image::sniff(&ihdr), Err(ImageError::Malformed("png IHDR")));
    // huge declared chunk length (no overflow / panic)
    let mut huge = png.clone();
    huge[33..37].copy_from_slice(&0xFFFF_FFE8u32.to_be_bytes());
    assert!(matches!(image::sniff(&huge), Err(ImageError::Malformed(_))));
    // JPEG: trailing bytes after EOI · SOF too short · truncated
    let jpg = fixture("b728x90.jpg");
    let mut jt = jpg.clone();
    jt.extend_from_slice(b"<html>");
    assert!(matches!(image::sniff(&jt), Err(ImageError::Malformed(_))));
    let sof_short = [0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x02, 0xFF, 0xD9];
    assert!(matches!(
        image::sniff(&sof_short),
        Err(ImageError::Malformed(_))
    ));
    assert!(matches!(
        image::sniff(&jpg[..40]),
        Err(ImageError::Malformed(_))
    ));
    // WebP: RIFF size mismatch · VP8X without image data
    let mut w = riff(&[(b"VP8L", vp8l(10, 10))]);
    w.push(0);
    assert_eq!(
        image::sniff(&w),
        Err(ImageError::Malformed("webp RIFF size"))
    );
    assert_eq!(
        image::sniff(&riff(&[(b"VP8X", vp8x(10, 10, 0))])),
        Err(ImageError::Malformed("webp without image data"))
    );
    // unknown / empty
    assert_eq!(image::sniff(b"GIF89a....."), Err(ImageError::UnknownType));
    assert_eq!(
        image::sniff(b"<svg xmlns=..."),
        Err(ImageError::UnknownType)
    );
    assert_eq!(image::sniff(&[]), Err(ImageError::Empty));
}

#[test]
fn rejects_zero_size() {
    assert_eq!(
        image::sniff(&riff(&[(b"VP8 ", vp8(0, 90))])),
        Err(ImageError::ZeroSize)
    );
}

#[test]
fn fuzz_does_not_panic() {
    // deterministic pseudo-random mutations of real files
    let seeds = [
        fixture("b728x90.png"),
        fixture("b728x90.jpg"),
        riff(&[(b"VP8X", vp8x(728, 90, 0)), (b"VP8L", vp8l(728, 90))]),
    ];
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for seed in &seeds {
        for _ in 0..20_000 {
            let mut b = seed.clone();
            for _ in 0..4 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let i = (x as usize) % b.len();
                b[i] = (x >> 32) as u8;
            }
            x ^= x << 13;
            b.truncate(1 + (x as usize) % b.len());
            let _ = image::sniff(&b);
        }
    }
}

#[test]
fn check_enforces_exact_size_and_byte_cap() {
    let png = fixture("b728x90.png");
    assert!(image::check(&png, 728, 90, 1 << 20).is_ok());
    assert_eq!(
        image::check(&png, 300, 250, 1 << 20),
        Err(ImageError::WrongSize {
            expected: (300, 250),
            got: (728, 90)
        })
    );
    assert_eq!(
        image::check(&png, 728, 90, 100),
        Err(ImageError::TooLarge {
            max: 100,
            got: png.len()
        })
    );
}

// ------------------------------------------------------------------- auth

fn message(
    game: &Pubkey,
    epoch: u64,
    slot: u8,
    payload: [u8; 32],
    nonce: &str,
    expires: i64,
) -> SignedMessage {
    SignedMessage {
        domain: DOMAIN.into(),
        cluster: "devnet".into(),
        program: ad_pool_id(),
        game: *game,
        epoch,
        slot,
        payload,
        nonce: nonce.into(),
        expires,
    }
}

#[test]
fn message_template_is_exact() {
    let g: Pubkey = "AJnPSYsn7J2muHfKztRtpZz5kBP94dWkNZLr9aMhu18G"
        .parse()
        .unwrap();
    let m = message(&g, 42, 3, [0xAB; 32], NONCE, NOW + 300);
    assert_eq!(
        m.render(),
        format!(
            "ad-pool creative submission\ndomain: ad-pool.devnet.local\ncluster: devnet\nprogram: 6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk\ngame: AJnPSYsn7J2muHfKztRtpZz5kBP94dWkNZLr9aMhu18G\nepoch: 42\nslot: 3\npayload: {}\nnonce: {NONCE}\nexpires: {}",
            "ab".repeat(32),
            NOW + 300
        )
    );
}

#[test]
fn verify_checks_nonce_expiry_and_signature() {
    let kp = Keypair::new();
    let g = Pubkey::new_unique();
    let m = message(&g, 1, 0, [9; 32], NONCE, NOW + 300);
    let sig = kp.sign_message(m.render().as_bytes()).to_string();
    assert_eq!(auth::verify(&m, &sig, &kp.pubkey(), NOW), Ok(()));
    assert_eq!(
        auth::verify(&m, &sig, &Pubkey::new_unique(), NOW),
        Err(AuthError::BadSignature)
    );
    assert_eq!(
        auth::verify(&m, "not-b58!", &kp.pubkey(), NOW),
        Err(AuthError::BadSignatureEncoding)
    );
    // the Worker-built message differs in any bound field → the signature no longer matches
    for other in [
        SignedMessage {
            domain: "evil.example".into(),
            ..m.clone()
        },
        SignedMessage {
            cluster: "mainnet".into(),
            ..m.clone()
        },
        SignedMessage {
            program: Pubkey::new_unique(),
            ..m.clone()
        },
        SignedMessage {
            game: Pubkey::new_unique(),
            ..m.clone()
        },
        SignedMessage {
            epoch: 2,
            ..m.clone()
        },
        SignedMessage {
            slot: 1,
            ..m.clone()
        },
        SignedMessage {
            payload: [1; 32],
            ..m.clone()
        },
    ] {
        assert_eq!(
            auth::verify(&other, &sig, &kp.pubkey(), NOW),
            Err(AuthError::BadSignature)
        );
    }
    // expiry / nonce checked before the signature
    let exp = message(&g, 1, 0, [9; 32], NONCE, NOW);
    assert_eq!(
        auth::verify(&exp, &sig, &kp.pubkey(), NOW),
        Err(AuthError::Expired)
    );
    let far = message(&g, 1, 0, [9; 32], NONCE, NOW + auth::MAX_VALIDITY_SECS + 1);
    assert_eq!(
        auth::verify(&far, &sig, &kp.pubkey(), NOW),
        Err(AuthError::TooLongValid)
    );
    for n in [
        "1234",
        "0123456789ABCDEF0123456789abcdef",
        "0123456789abcdef0123456789abcdeg",
    ] {
        let bad = message(&g, 1, 0, [9; 32], n, NOW + 300);
        assert_eq!(
            auth::verify(&bad, &sig, &kp.pubkey(), NOW),
            Err(AuthError::BadNonce)
        );
    }
}

#[test]
fn payload_hash_binds_every_part() {
    let base = auth::payload_hash(&[1; 32], "ข้อความ", "https://a.example");
    assert_ne!(
        base,
        auth::payload_hash(&[2; 32], "ข้อความ", "https://a.example")
    );
    assert_ne!(
        base,
        auth::payload_hash(&[1; 32], "ข้อความx", "https://a.example")
    );
    assert_ne!(
        base,
        auth::payload_hash(&[1; 32], "ข้อความ", "https://b.example")
    );
    assert_ne!(
        auth::payload_hash(&[1; 32], "ab", "c"),
        auth::payload_hash(&[1; 32], "a", "bc")
    );
}

// ------------------------------------------------------------- submission

struct Case {
    kp: Keypair,
    game: Pubkey,
    epoch: u64,
    slot: u8,
}

impl Case {
    fn new() -> Self {
        Case {
            kp: Keypair::new(),
            game: Pubkey::new_unique(),
            epoch: 8,
            slot: 0,
        }
    }
    fn input(&self, image: Vec<u8>, text: Option<&str>, click: Option<&str>) -> Input {
        let payload = auth::payload_hash(
            &auth::sha256(&image),
            text.unwrap_or(""),
            click.unwrap_or(""),
        );
        let m = message(&self.game, self.epoch, self.slot, payload, NONCE, NOW + 300);
        Input {
            game: self.game.to_string(),
            epoch: self.epoch.to_string(),
            slot: self.slot.to_string(),
            signer: self.kp.pubkey().to_string(),
            nonce: NONCE.into(),
            expires: (NOW + 300).to_string(),
            signature: self.kp.sign_message(m.render().as_bytes()).to_string(),
            image: Some(image),
            text: text.map(str::to_string),
            click_url: click.map(str::to_string),
        }
    }
    fn auction(&self, status: CreativeStatus, w: u16, h: u16) -> AdSlotAuction {
        AdSlotAuction {
            bump: 255,
            creative_status: status,
            settled: false,
            slot_index: self.slot,
            game_program: self.game,
            epoch: self.epoch,
            width: w,
            height: h,
            close_ts: NOW - 10,
            epoch_start_ts: NOW + 1_000,
            epoch_end_ts: NOW + 2_000,
            fee_bps: 100,
            min_increment_bps: 500,
            rent_payer: self.kp.pubkey(),
            highest_bidder: self.kp.pubkey(),
            highest_bid: 1,
            bid_count: 1,
            reject_count: 0,
            last_reject_flags: 0,
            approved_at: 0,
            updated_at: 0,
            creative: Creative::default(),
        }
    }
}

fn limits() -> Limits<'static> {
    let program: &'static Pubkey = Box::leak(Box::new(ad_pool_id()));
    Limits {
        domain: DOMAIN,
        cluster: "devnet",
        ad_pool: program,
        max_image_bytes: 1 << 20,
    }
}

#[test]
fn full_submission_happy_path() {
    let c = Case::new();
    let p = submission::prevalidate(
        c.input(
            fixture("b728x90.png"),
            Some("โฆษณา"),
            Some("https://example.com/x"),
        ),
        &limits(),
        NOW,
    )
    .unwrap();
    assert_eq!(p.kind, Kind::Full);
    assert_eq!(p.auction, pda::ad_slot(&ad_pool_id(), &c.game, 8, 0));
    assert_eq!(p.image_sha256, auth::sha256(&fixture("b728x90.png")));
    assert_eq!(p.message.nonce, NONCE);
    submission::against_auction(&p, &c.auction(CreativeStatus::Empty, 728, 90), NOW).unwrap();
}

#[test]
fn image_only_resubmission_requires_text_only_status() {
    let c = Case::new();
    let p = submission::prevalidate(c.input(fixture("b728x90.jpg"), None, None), &limits(), NOW)
        .unwrap();
    assert_eq!(p.kind, Kind::ImageOnly);
    submission::against_auction(&p, &c.auction(CreativeStatus::TextOnly, 728, 90), NOW).unwrap();
    assert_eq!(
        submission::against_auction(&p, &c.auction(CreativeStatus::Empty, 728, 90), NOW),
        Err(Reject::WrongKind {
            expected: Kind::Full
        })
    );
    let full = submission::prevalidate(
        c.input(fixture("b728x90.png"), Some("t"), Some("https://e.x")),
        &limits(),
        NOW,
    )
    .unwrap();
    assert_eq!(
        submission::against_auction(&full, &c.auction(CreativeStatus::TextOnly, 728, 90), NOW),
        Err(Reject::WrongKind {
            expected: Kind::ImageOnly
        })
    );
    assert_eq!(
        submission::against_auction(&full, &c.auction(CreativeStatus::Approved, 728, 90), NOW),
        Err(Reject::AlreadyApproved)
    );
}

#[test]
fn chain_checks_winner_window_size_and_identity() {
    let c = Case::new();
    let p = submission::prevalidate(
        c.input(fixture("b728x90.png"), Some("t"), Some("https://e.x")),
        &limits(),
        NOW,
    )
    .unwrap();
    let mut a = c.auction(CreativeStatus::Empty, 728, 90);
    a.highest_bidder = Pubkey::new_unique();
    assert_eq!(
        submission::against_auction(&p, &a, NOW),
        Err(Reject::NotWinner)
    );
    let a = c.auction(CreativeStatus::Empty, 728, 90);
    assert_eq!(
        submission::against_auction(&p, &a, a.close_ts - 1),
        Err(Reject::NotInCreativeWindow)
    );
    assert_eq!(
        submission::against_auction(&p, &a, a.epoch_end_ts),
        Err(Reject::NotInCreativeWindow)
    );
    assert!(
        submission::against_auction(&p, &a, a.epoch_end_ts - 1).is_ok(),
        "Live is allowed (Q5)"
    );
    assert!(matches!(
        submission::against_auction(&p, &c.auction(CreativeStatus::Empty, 300, 250), NOW),
        Err(Reject::Image(ImageError::WrongSize { .. }))
    ));
    let mut other = c.auction(CreativeStatus::Empty, 728, 90);
    other.epoch = 9;
    assert_eq!(
        submission::against_auction(&p, &other, NOW),
        Err(Reject::AuctionMismatch)
    );
}

#[test]
fn prevalidate_rejects_bad_fields() {
    let c = Case::new();
    let png = fixture("b728x90.png");
    let thai_121 = format!("{}x", "ก".repeat(40));
    for t in [
        String::new(),
        thai_121,
        "line\nbreak".into(),
        "tab\tx".into(),
        "ok\u{202E}evil".into(),
        "zero\u{200B}width".into(),
    ] {
        assert_eq!(
            submission::prevalidate(
                c.input(png.clone(), Some(&t), Some("https://e.x")),
                &limits(),
                NOW
            )
            .unwrap_err(),
            Reject::BadField("text"),
            "{t:?}"
        );
    }
    let long_url = format!("https://{}", "a".repeat(193));
    for u in [
        "http://e.x",
        "https://",
        "HTTPS://e.x",
        "https://e.x/ a",
        "javascript:alert(1)",
        long_url.as_str(),
    ] {
        assert_eq!(
            submission::prevalidate(c.input(png.clone(), Some("t"), Some(u)), &limits(), NOW)
                .unwrap_err(),
            Reject::BadField("click_url"),
            "{u}"
        );
    }
    assert_eq!(
        submission::prevalidate(c.input(png.clone(), Some("t"), None), &limits(), NOW).unwrap_err(),
        Reject::BadField("text and click_url go together")
    );
    let mut i = c.input(png.clone(), Some("t"), Some("https://e.x"));
    i.image = None;
    assert_eq!(
        submission::prevalidate(i, &limits(), NOW).unwrap_err(),
        Reject::BadField("image")
    );
    let mut i = c.input(png.clone(), Some("t"), Some("https://e.x"));
    i.epoch = "-1".into();
    assert_eq!(
        submission::prevalidate(i, &limits(), NOW).unwrap_err(),
        Reject::BadField("epoch")
    );
    let mut i = c.input(png.clone(), Some("t"), Some("https://e.x"));
    i.expires = "soon".into();
    assert_eq!(
        submission::prevalidate(i, &limits(), NOW).unwrap_err(),
        Reject::BadField("expires")
    );
    let mut small = limits();
    small.max_image_bytes = 10;
    assert!(matches!(
        submission::prevalidate(
            c.input(png.clone(), Some("t"), Some("https://e.x")),
            &small,
            NOW
        )
        .unwrap_err(),
        Reject::Image(ImageError::TooLarge { .. })
    ));
    assert_eq!(
        submission::prevalidate(
            c.input(b"GIF89a".to_vec(), Some("t"), Some("https://e.x")),
            &limits(),
            NOW
        )
        .unwrap_err(),
        Reject::Image(ImageError::UnknownType)
    );
}

#[test]
fn signature_must_cover_the_submitted_bytes_and_auction() {
    let c = Case::new();
    let png = fixture("b728x90.png");
    let bad = Reject::Auth(AuthError::BadSignature);
    let mut i = c.input(png.clone(), Some("t"), Some("https://e.x"));
    i.image = Some(fixture("b728x90.jpg"));
    assert_eq!(
        submission::prevalidate(i, &limits(), NOW).unwrap_err(),
        bad,
        "swapped image"
    );
    let mut i = c.input(png.clone(), Some("t"), Some("https://e.x"));
    i.text = Some("other".into());
    assert_eq!(
        submission::prevalidate(i, &limits(), NOW).unwrap_err(),
        bad,
        "swapped text"
    );
    let mut i = c.input(png.clone(), Some("t"), Some("https://e.x"));
    i.slot = "1".into();
    assert_eq!(
        submission::prevalidate(i, &limits(), NOW).unwrap_err(),
        bad,
        "other slot"
    );
    let mut i = c.input(png.clone(), Some("t"), Some("https://e.x"));
    i.nonce = "fedcba9876543210fedcba9876543210".into();
    assert_eq!(
        submission::prevalidate(i, &limits(), NOW).unwrap_err(),
        bad,
        "other nonce"
    );
    let mut i = c.input(png.clone(), Some("t"), Some("https://e.x"));
    i.signer = Pubkey::new_unique().to_string();
    assert_eq!(
        submission::prevalidate(i, &limits(), NOW).unwrap_err(),
        bad,
        "other signer"
    );
    // same request on another deployment (domain) fails
    let mut other_site = limits();
    other_site.domain = "evil.example";
    assert_eq!(
        submission::prevalidate(
            c.input(png, Some("t"), Some("https://e.x")),
            &other_site,
            NOW
        )
        .unwrap_err(),
        bad
    );
}

#[test]
fn reject_status_codes() {
    assert_eq!(Reject::BadField("x").status(), 400);
    assert_eq!(Reject::Auth(AuthError::BadSignature).status(), 401);
    assert_eq!(Reject::NotWinner.status(), 403);
    assert_eq!(Reject::AlreadyApproved.status(), 409);
}

// ------------------------------------------------------------------- gate

#[test]
fn gate_blocks_replay_and_rate_limits_per_window() {
    let s = GateState::default();
    let s = gate::admit(s, "n1", NOW + 600, NOW, 2).unwrap();
    assert_eq!(
        gate::admit(s.clone(), "n1", NOW + 600, NOW + 1, 2),
        Err(GateError::Replay)
    );
    let s = gate::admit(s, "n2", NOW + 600, NOW + 2, 2).unwrap();
    assert_eq!(
        gate::admit(s.clone(), "n3", NOW + 600, NOW + 3, 2),
        Err(GateError::RateLimited {
            retry_after_secs: gate::WINDOW_SECS - 3
        })
    );
    let later = NOW + gate::WINDOW_SECS + 1;
    let s = gate::admit(s, "n3", later + 600, later, 2).unwrap();
    assert!(
        !s.nonces.iter().any(|(n, _)| n == "n1"),
        "expired nonce pruned"
    );
    assert_eq!(s.recent.len(), 2);
}

// ------------------------------------------------------- round-2 regressions

/// Minimal JPEG: SOI · SOFn(w,h,1 comp) · SOS · data · [extra scan] · EOI.
fn jpeg_min(sof: u8, w: u16, h: u16, scans: usize) -> Vec<u8> {
    let mut j = vec![0xFF, 0xD8];
    j.extend_from_slice(&[0xFF, sof, 0x00, 0x0B, 0x08]);
    j.extend_from_slice(&h.to_be_bytes());
    j.extend_from_slice(&w.to_be_bytes());
    j.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
    for _ in 0..scans {
        j.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        // entropy data incl. a stuffed FF00 and a restart marker
        j.extend_from_slice(&[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56]);
    }
    j.extend_from_slice(&[0xFF, 0xD9]);
    j
}

#[test]
fn jpeg_progressive_multi_scan_is_accepted() {
    let i = image::sniff(&jpeg_min(0xC2, 728, 90, 3)).unwrap();
    assert_eq!((i.kind, i.width, i.height), (ImageKind::Jpeg, 728, 90));
}

#[test]
fn jpeg_content_after_first_eoi_is_rejected() {
    // decoders stop at the first EOI: the rest would be smuggled (HTML, ZIP …)
    let mut j = fixture("b728x90.jpg");
    j.extend_from_slice(b"<html><script>alert(1)</script>PK\x03\x04");
    j.extend_from_slice(&[0xFF, 0xD9]);
    assert_eq!(
        image::sniff(&j),
        Err(ImageError::Malformed("jpeg trailing bytes"))
    );
}

#[test]
fn jpeg_second_sof_and_long_orientation() {
    let mut two = jpeg_min(0xC0, 728, 90, 1);
    let sof = two[2..15].to_vec();
    two.splice(15..15, sof);
    assert_eq!(
        image::sniff(&two),
        Err(ImageError::Malformed("jpeg second SOF"))
    );
    // Orientation stored as LONG = 1 is fine, LONG = 6 is rejected
    let long = |v: u32| {
        let mut t = b"MM\0*".to_vec();
        t.extend_from_slice(&8u32.to_be_bytes());
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&0x0112u16.to_be_bytes());
        t.extend_from_slice(&4u16.to_be_bytes());
        t.extend_from_slice(&1u32.to_be_bytes());
        t.extend_from_slice(&v.to_be_bytes());
        t.extend_from_slice(&0u32.to_be_bytes());
        t
    };
    assert!(image::sniff(&jpeg_with_exif(long(1))).is_ok());
    assert_eq!(
        image::sniff(&jpeg_with_exif(long(6))),
        Err(ImageError::Exif)
    );
}

#[test]
fn xmp_gps_is_rejected_in_every_container() {
    let xmp_gps =
        b"<x:xmpmeta><rdf:Description exif:GPSLatitude=\"13,45.0N\"/></x:xmpmeta>".to_vec();
    let xmp_ok = b"<x:xmpmeta><rdf:Description xmp:CreatorTool=\"Figma\"/></x:xmpmeta>".to_vec();
    // JPEG APP1 XMP
    let app1 = |x: &[u8]| {
        let j = fixture("b728x90.jpg");
        let mut seg = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        seg.extend_from_slice(x);
        let mut out = j[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((seg.len() + 2) as u16).to_be_bytes());
        out.extend(seg);
        out.extend_from_slice(&j[2..]);
        out
    };
    assert_eq!(image::sniff(&app1(&xmp_gps)), Err(ImageError::Exif));
    assert!(image::sniff(&app1(&xmp_ok)).is_ok());
    // PNG iTXt XMP (uncompressed) and compressed XMP (cannot inspect → reject)
    let itxt = |flag: u8, x: &[u8]| {
        let mut d = b"XML:com.adobe.xmp\0".to_vec();
        d.extend_from_slice(&[flag, 0, 0, 0]);
        d.extend_from_slice(x);
        png_with_chunk(b"iTXt", &d)
    };
    assert_eq!(image::sniff(&itxt(0, &xmp_gps)), Err(ImageError::Exif));
    assert!(image::sniff(&itxt(0, &xmp_ok)).is_ok());
    assert_eq!(image::sniff(&itxt(1, &xmp_ok)), Err(ImageError::Exif));
    // WebP XMP chunk
    assert_eq!(
        image::sniff(&riff(&[
            (b"VP8X", vp8x(728, 90, 0)),
            (b"VP8L", vp8l(728, 90)),
            (b"XMP ", xmp_gps.clone())
        ])),
        Err(ImageError::Exif)
    );
}

#[test]
fn webp_canvas_must_match_frame_and_single_image() {
    assert_eq!(
        image::sniff(&riff(&[(b"VP8X", vp8x(728, 90, 0)), (b"VP8L", vp8l(1, 1))])),
        Err(ImageError::Malformed("webp canvas != frame"))
    );
    assert_eq!(
        image::sniff(&riff(&[(b"VP8L", vp8l(10, 10)), (b"VP8L", vp8l(10, 10))])),
        Err(ImageError::Malformed("webp second image"))
    );
}

#[test]
fn webp_max_chunk_length_is_an_error_not_a_panic() {
    // the wasm32 trap Fable found (len = u32::MAX wrapped `len + 1`); also run as wasm32 by
    // scripts/wasm-fuzz.sh
    let mut w = b"RIFF".to_vec();
    w.extend_from_slice(&12u32.to_le_bytes());
    w.extend_from_slice(b"WEBPVP8L");
    w.extend_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(image::sniff(&w), Err(ImageError::Malformed(_))));
}

#[test]
fn replay_and_rate_limit_have_distinct_errors() {
    let s = gate::admit(GateState::default(), "n1", NOW + 600, NOW, 10).unwrap();
    assert_eq!(
        gate::admit(s, "n1", NOW + 600, NOW, 10),
        Err(GateError::Replay)
    );
    // GateState round-trips through serde (DO storage) as plain arrays
    let s = gate::admit(GateState::default(), "n2", NOW + 600, NOW, 10).unwrap();
    let json = serde_json::to_string(&s).unwrap();
    assert!(json.starts_with("{\"nonces\":[["), "{json}");
    assert_eq!(serde_json::from_str::<GateState>(&json).unwrap(), s);
}

#[test]
fn rejects_more_invisible_characters_and_maps_auction_mismatch_to_502() {
    let c = Case::new();
    for t in ["a\u{061C}b", "a\u{2028}b", "a\u{FFF9}b"] {
        assert_eq!(
            submission::prevalidate(
                c.input(fixture("b728x90.png"), Some(t), Some("https://e.x")),
                &limits(),
                NOW
            )
            .unwrap_err(),
            Reject::BadField("text")
        );
    }
    assert_eq!(Reject::AuctionMismatch.status(), 502);
}
