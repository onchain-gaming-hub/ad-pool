//! Dev helper: sign a submission and print curl args (signer, signature, nonce, expires —
//! the Worker rebuilds the message itself).
//!   creative:   cargo run -q --example sign -- <image> <game> <epoch> <slot> [text] [click_url]
//!   default ad: SIGN_MODE=default-ad cargo run -q --example sign -- <image> <width> <height> <text> <click_url>
//! Signs with a fresh keypair, or with `SIGNER_KEYPAIR=<solana keypair .json>` (devnet smoke).
use ad_pool_worker::auth::{self, DefaultAdMessage, SignedMessage};
use solana_keypair::Keypair;
use solana_signer::Signer;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let image = std::fs::read(&a[1]).unwrap();
    let default_ad = std::env::var("SIGN_MODE").as_deref() == Ok("default-ad");
    let (text, click) = if default_ad {
        (a[4].clone(), a[5].clone())
    } else {
        (
            a.get(5).cloned().unwrap_or_default(),
            a.get(6).cloned().unwrap_or_default(),
        )
    };
    let payload = auth::payload_hash(&auth::sha256(&image), &text, &click);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let nonce: String = Keypair::new().pubkey().to_bytes()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let domain = std::env::var("AUTH_DOMAIN").unwrap_or("ad-pool.devnet.local".into());
    let expires = now + 300;
    let rendered = if default_ad {
        DefaultAdMessage {
            domain,
            cluster: "devnet".into(),
            program: "AJnPSYsn7J2muHfKztRtpZz5kBP94dWkNZLr9aMhu18G"
                .parse()
                .unwrap(),
            width: a[2].parse().unwrap(),
            height: a[3].parse().unwrap(),
            payload,
            nonce: nonce.clone(),
            expires,
        }
        .render()
    } else {
        SignedMessage {
            domain,
            cluster: "devnet".into(),
            program: "6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk"
                .parse()
                .unwrap(),
            game: a[2].parse().unwrap(),
            epoch: a[3].parse().unwrap(),
            slot: a[4].parse().unwrap(),
            payload,
            nonce: nonce.clone(),
            expires,
        }
        .render()
    };
    let kp = match std::env::var("SIGNER_KEYPAIR") {
        Ok(p) => {
            let bytes: Vec<u8> =
                serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
            Keypair::try_from(bytes.as_slice()).unwrap()
        }
        Err(_) => Keypair::new(),
    };
    println!(
        "{}\n{}\n{}\n{}",
        kp.pubkey(),
        kp.sign_message(rendered.as_bytes()),
        nonce,
        expires
    );
}
