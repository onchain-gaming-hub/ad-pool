//! Dev helper: sign a creative submission and print curl args.
//!   cargo run -q --example sign -- <image> <game> <epoch> <slot> [text] [click_url]
//! Signs with a fresh keypair, or with `SIGNER_KEYPAIR=<solana keypair .json>` (devnet smoke).
use ad_pool_worker::auth::{self, SignedMessage};
use solana_keypair::Keypair;
use solana_signer::Signer;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let image = std::fs::read(&a[1]).unwrap();
    let (text, click) = (
        a.get(5).cloned().unwrap_or_default(),
        a.get(6).cloned().unwrap_or_default(),
    );
    let payload = auth::payload_hash(&auth::sha256(&image), &text, &click);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let nonce: String = Keypair::new().pubkey().to_bytes()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let m = SignedMessage {
        domain: std::env::var("AUTH_DOMAIN").unwrap_or("ad-pool.devnet.local".into()),
        cluster: "devnet".into(),
        program: "6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk"
            .parse()
            .unwrap(),
        game: a[2].parse().unwrap(),
        epoch: a[3].parse().unwrap(),
        slot: a[4].parse().unwrap(),
        payload,
        nonce,
        expires: now + 300,
    };
    let kp = match std::env::var("SIGNER_KEYPAIR") {
        Ok(p) => {
            let bytes: Vec<u8> = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
            Keypair::try_from(bytes.as_slice()).unwrap()
        }
        Err(_) => Keypair::new(),
    };
    let text_msg = m.render();
    // prints: signer, signature, nonce, expires (the Worker rebuilds the message itself)
    println!(
        "{}\n{}\n{}\n{}",
        kp.pubkey(),
        kp.sign_message(text_msg.as_bytes()),
        m.nonce,
        m.expires
    );
}
