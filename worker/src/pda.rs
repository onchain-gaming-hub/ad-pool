//! PDA derivations — same seeds as the program (constants.rs) and platform-core (01 §9).

use solana_pubkey::Pubkey;

use crate::accounts::Role;

pub const AD_CONFIG_SEED: &[u8] = b"ad_config";
pub const AD_SLOT_SEED: &[u8] = b"ad_slot";
pub const PC_GAME_SEED: &[u8] = b"game";
pub const PC_CONFIG_SEED: &[u8] = b"config";
pub const PC_ROLE_SEED: &[u8] = b"role";
pub const PC_TREASURY_SEED: &[u8] = b"treasury";

pub fn ad_config(ad_pool: &Pubkey, game_program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[AD_CONFIG_SEED, game_program.as_ref()], ad_pool).0
}

pub fn ad_slot(ad_pool: &Pubkey, game_program: &Pubkey, epoch: u64, slot_index: u8) -> Pubkey {
    Pubkey::find_program_address(
        &[
            AD_SLOT_SEED,
            game_program.as_ref(),
            &epoch.to_le_bytes(),
            &[slot_index],
        ],
        ad_pool,
    )
    .0
}

pub fn game_entry(platform_core: &Pubkey, game_program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[PC_GAME_SEED, game_program.as_ref()], platform_core).0
}

pub fn role_member(platform_core: &Pubkey, role: Role, member: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[PC_ROLE_SEED, &[role as u8], member.as_ref()],
        platform_core,
    )
    .0
}

pub fn platform_config(platform_core: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[PC_CONFIG_SEED], platform_core).0
}

pub fn treasury(platform_core: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[PC_TREASURY_SEED], platform_core).0
}

pub const PC_DEFAULT_AD_SEED: &[u8] = b"default_ad";

/// platform-core `DefaultAd` `[b"default_ad", width LE, height LE]` (pc 01 §7).
pub fn default_ad(platform_core: &Pubkey, width: u16, height: u16) -> Pubkey {
    Pubkey::find_program_address(
        &[
            PC_DEFAULT_AD_SEED,
            &width.to_le_bytes(),
            &height.to_le_bytes(),
        ],
        platform_core,
    )
    .0
}
