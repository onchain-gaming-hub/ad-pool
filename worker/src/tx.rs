//! Moderator transactions (W2): instruction data for `approve_creative` / `approve_image` /
//! `reject_creative` (design §7.1–7.3), a legacy message with the Moderator key as the only
//! signer and fee payer, signed and wire-encoded. Pure — golden-tested natively against the
//! program's own Anchor types (tests/golden.rs).

use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

use crate::{accounts::Role, pda};

// Anchor discriminators = sha256("global:<name>")[..8] (idls/ad_pool.json).
pub const APPROVE_CREATIVE_DISCRIMINATOR: [u8; 8] = [111, 12, 141, 173, 1, 3, 255, 94];
pub const APPROVE_IMAGE_DISCRIMINATOR: [u8; 8] = [114, 23, 242, 150, 209, 27, 43, 147];
pub const REJECT_CREATIVE_DISCRIMINATOR: [u8; 8] = [121, 206, 31, 210, 38, 54, 232, 60];
pub const REVOKE_CREATIVE_DISCRIMINATOR: [u8; 8] = [84, 177, 210, 233, 240, 176, 2, 166];
pub const SETTLE_DISCRIMINATOR: [u8; 8] = [175, 42, 185, 87, 144, 131, 102, 212];
pub const CLOSE_AUCTION_DISCRIMINATOR: [u8; 8] = [225, 129, 91, 48, 215, 73, 203, 172];

/// Reason bits for `reject_creative.flags` / `image_reject_flags` / `last_reject_flags`
/// (design §7.3). The Worker always sends at least one bit (the program accepts 0).
pub mod flags {
    pub const FILE: u32 = 1 << 0;
    pub const SIZE: u32 = 1 << 1;
    pub const IMAGE_MODERATION: u32 = 1 << 2;
    pub const TEXT_MODERATION: u32 = 1 << 3;
    pub const URL_LAYER1: u32 = 1 << 4;
    pub const URL_LAYER2: u32 = 1 << 5;
    pub const TOO_LONG: u32 = 1 << 6;
}

/// What the Worker sends on chain after moderation (design §7.5).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Action {
    /// `approve_creative` with image → `Approved`
    ApproveFull {
        image_url: String,
        image_sha256: [u8; 32],
        text: String,
        click_url: String,
    },
    /// `approve_creative` without image → `TextOnly`; the image's reasons recorded in the same tx
    ApproveTextOnly {
        text: String,
        click_url: String,
        image_reject_flags: u32,
    },
    /// `approve_image` on a `TextOnly` auction → `Approved`
    ApproveImage {
        image_url: String,
        image_sha256: [u8; 32],
    },
    /// `reject_creative` — status unchanged, reasons on chain
    Reject { flags: u32 },
    /// `revoke_creative` (W3 re-check) — `TextOnly`/`Approved` → `Empty`, reasons on chain
    Revoke { flags: u32 },
}

fn borsh_string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// `AdCreative` field order: image_url, image_sha256, text, click_url (IDL).
fn creative(out: &mut Vec<u8>, image_url: &str, sha: &[u8; 32], text: &str, click_url: &str) {
    borsh_string(out, image_url);
    out.extend_from_slice(sha);
    borsh_string(out, text);
    borsh_string(out, click_url);
}

pub fn ix_data(action: &Action) -> Vec<u8> {
    let mut d = Vec::with_capacity(8 + 4 + 200 + 32 + 4 + 120 + 4 + 200 + 4);
    match action {
        Action::ApproveFull {
            image_url,
            image_sha256,
            text,
            click_url,
        } => {
            d.extend_from_slice(&APPROVE_CREATIVE_DISCRIMINATOR);
            creative(&mut d, image_url, image_sha256, text, click_url);
            d.extend_from_slice(&0u32.to_le_bytes());
        }
        Action::ApproveTextOnly {
            text,
            click_url,
            image_reject_flags,
        } => {
            d.extend_from_slice(&APPROVE_CREATIVE_DISCRIMINATOR);
            creative(&mut d, "", &[0; 32], text, click_url);
            d.extend_from_slice(&image_reject_flags.to_le_bytes());
        }
        Action::ApproveImage {
            image_url,
            image_sha256,
        } => {
            d.extend_from_slice(&APPROVE_IMAGE_DISCRIMINATOR);
            borsh_string(&mut d, image_url);
            d.extend_from_slice(image_sha256);
        }
        Action::Reject { flags } => {
            d.extend_from_slice(&REJECT_CREATIVE_DISCRIMINATOR);
            d.extend_from_slice(&flags.to_le_bytes());
        }
        Action::Revoke { flags } => {
            d.extend_from_slice(&REVOKE_CREATIVE_DISCRIMINATOR);
            d.extend_from_slice(&flags.to_le_bytes());
        }
    }
    d
}

/// All three moderator ixs share one account list: moderator (signer, mut — fee payer),
/// `RoleMember(Moderator, moderator)` of platform-core, auction (mut).
pub fn instruction(
    ad_pool: &Pubkey,
    platform_core: &Pubkey,
    moderator: &Pubkey,
    auction: &Pubkey,
    action: &Action,
) -> Instruction {
    Instruction::new_with_bytes(
        *ad_pool,
        &ix_data(action),
        vec![
            AccountMeta::new(*moderator, true),
            AccountMeta::new_readonly(
                pda::role_member(platform_core, Role::Moderator, moderator),
                false,
            ),
            AccountMeta::new(*auction, false),
        ],
    )
}

/// `settle` (permissionless, W3 crank): no signer in the ix — the crank key only pays the fee.
/// `game_owner` = `GameEntry.owner` read at settle time (the program checks the address).
pub fn settle_instruction(
    ad_pool: &Pubkey,
    platform_core: &Pubkey,
    auction: &Pubkey,
    game_program: &Pubkey,
    game_owner: &Pubkey,
) -> Instruction {
    Instruction::new_with_bytes(
        *ad_pool,
        &SETTLE_DISCRIMINATOR,
        vec![
            AccountMeta::new(*auction, false),
            AccountMeta::new_readonly(pda::game_entry(platform_core, game_program), false),
            AccountMeta::new(*game_owner, false),
            AccountMeta::new_readonly(pda::platform_config(platform_core), false),
            AccountMeta::new(pda::treasury(platform_core), false),
        ],
    )
}

/// `close_auction` (permissionless pure close): rent goes to the stored `rent_payer` only.
pub fn close_instruction(ad_pool: &Pubkey, auction: &Pubkey, rent_payer: &Pubkey) -> Instruction {
    Instruction::new_with_bytes(
        *ad_pool,
        &CLOSE_AUCTION_DISCRIMINATOR,
        vec![
            AccountMeta::new(*auction, false),
            AccountMeta::new(*rent_payer, false),
        ],
    )
}

/// Legacy message, Moderator = fee payer + only signer → wire bytes (what `sendTransaction`
/// takes base64-encoded) and the signature (base58 = the tx id).
pub fn sign(
    moderator: &Keypair,
    ix: Instruction,
    blockhash: &Hash,
) -> Result<(Vec<u8>, String), String> {
    sign_many(moderator, &[ix], blockhash)
}

/// Same, several ixs (crank: `settle` + `close_auction` in one tx). `payer` must be the only
/// signer — no ix may require another signature.
pub fn sign_many(
    payer: &Keypair,
    ixs: &[Instruction],
    blockhash: &Hash,
) -> Result<(Vec<u8>, String), String> {
    let msg = Message::new_with_blockhash(ixs, Some(&payer.pubkey()), blockhash);
    if msg.header.num_required_signatures != 1 {
        return Err("tx must have exactly one signer".into());
    }
    let message = VersionedMessage::Legacy(msg);
    let signature = payer.sign_message(&message.serialize());
    let tx = VersionedTransaction {
        signatures: vec![signature],
        message,
    };
    let bytes = wincode::serialize(&tx).map_err(|e| format!("encode: {e}"))?;
    if bytes.len() > 1232 {
        return Err(format!("tx {} bytes > 1232", bytes.len()));
    }
    Ok((bytes, signature.to_string()))
}

/// `MODERATOR_KEYPAIR` / `CRANK_KEYPAIR` secret: base58 of the 64-byte keypair.
pub fn keypair_from_base58(s: &str) -> Result<Keypair, String> {
    let bytes = bs58::decode(s.trim())
        .into_vec()
        .map_err(|_| "keypair secret is not base58".to_string())?;
    if bytes.len() != 64 {
        return Err("keypair secret must be 64 bytes".into());
    }
    Keypair::try_from(bytes.as_slice()).map_err(|_| "keypair secret invalid".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_layouts() {
        let r = ix_data(&Action::Reject { flags: 0x24 });
        assert_eq!(&r[..8], &REJECT_CREATIVE_DISCRIMINATOR);
        assert_eq!(&r[8..], &[0x24, 0, 0, 0]);
        let t = ix_data(&Action::ApproveTextOnly {
            text: "ab".into(),
            click_url: "https://x.io/".into(),
            image_reject_flags: 4,
        });
        // disc + "" + 32 zero + "ab" + url + flags
        assert_eq!(t.len(), 8 + 4 + 32 + 4 + 2 + 4 + 13 + 4);
        assert_eq!(&t[t.len() - 4..], &[4, 0, 0, 0]);
    }

    #[test]
    fn signs_a_single_signer_tx_under_the_limit() {
        let kp = Keypair::new();
        let ad_pool = Pubkey::new_unique();
        let pc = Pubkey::new_unique();
        let auction = Pubkey::new_unique();
        // maximum-size creative still fits (design §9)
        let a = Action::ApproveFull {
            image_url: "u".repeat(200),
            image_sha256: [9; 32],
            text: "t".repeat(120),
            click_url: format!("https://{}", "c".repeat(192)),
        };
        let ix = instruction(&ad_pool, &pc, &kp.pubkey(), &auction, &a);
        let (bytes, sig) = sign(&kp, ix, &Hash::new_from_array([7; 32])).unwrap();
        assert!(bytes.len() <= 1232, "{}", bytes.len());
        let tx: VersionedTransaction = wincode::deserialize_exact(&bytes).unwrap();
        assert_eq!(tx.signatures[0].to_string(), sig);
        assert_eq!(tx.message.static_account_keys()[0], kp.pubkey());
        assert!(tx.signatures[0].verify(kp.pubkey().as_ref(), &tx.message.serialize()));
    }

    #[test]
    fn keypair_secret_parsing() {
        let kp = Keypair::new();
        let s = bs58::encode(kp.to_bytes()).into_string();
        assert_eq!(keypair_from_base58(&s).unwrap().pubkey(), kp.pubkey());
        assert!(keypair_from_base58("0OIl").is_err());
        assert!(keypair_from_base58(&bs58::encode([1u8; 32]).into_string()).is_err());
    }
}
