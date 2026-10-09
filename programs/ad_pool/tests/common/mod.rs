//! Shared LiteSVM helpers. platform-core accounts are written straight into the SVM
//! (owner = platform-core ID, Anchor discriminator + borsh from the `declare_program!`
//! types) — `Account<T>` only checks owner + discriminator + seeds, so the pc .so is
//! not needed. ⚠️ Loads the PRE-BUILT target/deploy/ad_pool.so: run ./scripts/test.sh.
#![allow(dead_code)]
#![allow(clippy::result_large_err)]

use {
    ad_pool::{
        constants::*,
        instructions::AdSlotSpecInput,
        platform_core::{
            self,
            accounts::{GameEntry, PlatformConfig, RoleMember},
            types::{GameStatus, Role},
        },
    },
    anchor_lang::{
        prelude::{Clock, Pubkey},
        solana_program::{instruction::Instruction, system_program},
        AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas,
    },
    litesvm::{types::TransactionResult, LiteSVM},
    solana_account::Account,
    solana_keypair::Keypair,
    solana_message::{Message, VersionedMessage},
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
};

pub const SOL: u64 = 1_000_000_000;
pub const DAY: i64 = 86_400;
/// A UTC midnight in 2026 — used as genesis and as "now" baseline.
pub const T0: i64 = 1_790_000_000 - 1_790_000_000 % DAY;

pub fn program_so(name: &str) -> Vec<u8> {
    let path = format!("{}/../deploy/{name}.so", env!("CARGO_TARGET_TMPDIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e} — build first: ./scripts/test.sh"))
}

pub fn setup() -> LiteSVM {
    let mut svm = LiteSVM::new();
    svm.add_program(ad_pool::id(), &program_so("ad_pool"))
        .unwrap();
    set_time(&mut svm, T0);
    put_pc_config(&mut svm, 100); // ad fee 1% (rev 7)
    svm
}

pub fn funded(svm: &mut LiteSVM) -> Keypair {
    let kp = Keypair::new();
    svm.airdrop(&kp.pubkey(), 100 * SOL).unwrap();
    kp
}

pub fn set_time(svm: &mut LiteSVM, unix_timestamp: i64) {
    let mut clock: Clock = svm.get_sysvar();
    clock.unix_timestamp = unix_timestamp;
    svm.set_sysvar(&clock);
}

/// Rent-exempt minimum as the runtime computes it (LiteSVM uses solana-rent 4.x, the
/// program's `Rent` type is 3.x — compare numbers, not types).
pub fn rent_min(svm: &LiteSVM, len: usize) -> u64 {
    svm.minimum_balance_for_rent_exemption(len)
}

/// Same formula as `ad_pool::math::min_bid_floor`, on the SVM's rent.
pub fn floor(svm: &LiteSVM) -> u64 {
    let storage = rent_min(svm, AD_SLOT_AUCTION_SIZE as usize);
    let zero = rent_min(svm, 0) as u128;
    let payout =
        (zero * BPS_DENOMINATOR as u128).div_ceil((BPS_DENOMINATOR - MAX_FEE_BPS) as u128) as u64;
    storage.max(payout)
}

pub fn send(svm: &mut LiteSVM, ixs: &[Instruction], signers: &[&Keypair]) -> TransactionResult {
    let blockhash = svm.latest_blockhash();
    let msg = Message::new_with_blockhash(ixs, Some(&signers[0].pubkey()), &blockhash);
    let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), signers).unwrap();
    let res = svm.send_transaction(tx);
    svm.expire_blockhash();
    res
}

pub fn assert_ok(res: &TransactionResult) {
    if let Err(e) = res {
        panic!("tx failed: {:?}\nlogs: {:#?}", e.err, e.meta.logs);
    }
}

pub fn assert_custom_err(res: &TransactionResult, code: u32) {
    match res {
        Ok(_) => panic!("expected Custom({code}), tx succeeded"),
        Err(e) => {
            let s = format!("{:?}", e.err);
            assert!(
                s.contains(&format!("Custom({code})")),
                "expected Custom({code}), got {s}\nlogs: {:#?}",
                e.meta.logs
            );
        }
    }
}

/// Program error code of an `AdPoolError`.
pub fn code(e: ad_pool::error::AdPoolError) -> u32 {
    u32::from(e)
}

pub fn fetch<T: AccountDeserialize>(svm: &LiteSVM, address: &Pubkey) -> T {
    let account = svm.get_account(address).expect("account exists");
    T::try_deserialize(&mut account.data.as_slice()).unwrap()
}

/// Every event of type `T` in the tx logs (`emit!` = `Program data: base64(disc ‖ borsh)`).
pub fn events<T: anchor_lang::Event + anchor_lang::AnchorDeserialize>(
    res: &TransactionResult,
) -> Vec<T> {
    use base64::Engine;
    logs_of(res)
        .iter()
        .filter_map(|l| l.strip_prefix("Program data: "))
        .filter_map(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
        .filter(|bytes| bytes.starts_with(T::DISCRIMINATOR))
        .map(|bytes| T::deserialize(&mut &bytes[T::DISCRIMINATOR.len()..]).unwrap())
        .collect()
}

pub fn lamports(svm: &LiteSVM, address: &Pubkey) -> u64 {
    svm.get_account(address).map(|a| a.lamports).unwrap_or(0)
}

/// Base fee of a tx with `signatures` signatures (LiteSVM default 5,000 / sig).
pub fn base_fee(signatures: u64) -> u64 {
    5_000 * signatures
}

/// `ManageAdConfig` metas with an explicit GameEntry (cross-game substitution tests).
pub fn ix_add_slot_with_entry(
    game_owner: &Pubkey,
    game_program: &Pubkey,
    game_entry: &Pubkey,
) -> Instruction {
    Instruction::new_with_bytes(
        ad_pool::id(),
        &ad_pool::instruction::AddSlot {
            width: 1,
            height: 1,
        }
        .data(),
        ad_pool::accounts::ManageAdConfig {
            game_owner: *game_owner,
            game_entry: *game_entry,
            ad_config: ad_config_pda(game_program),
        }
        .to_account_metas(None),
    )
}

pub fn logs_of(res: &TransactionResult) -> Vec<String> {
    match res {
        Ok(m) => m.logs.clone(),
        Err(e) => e.meta.logs.clone(),
    }
}

// --------------------------------------------------- platform-core fixtures

fn put_pc_account<T: AccountSerialize>(svm: &mut LiteSVM, address: Pubkey, value: &T) {
    let mut data = Vec::new();
    value.try_serialize(&mut data).unwrap();
    let lamports = rent_min(svm, data.len());
    svm.set_account(
        address,
        Account {
            lamports,
            data,
            owner: platform_core::ID,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap();
}

pub fn game_entry_pda(game_program: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[PC_GAME_SEED, game_program.as_ref()], &platform_core::ID)
}

pub fn pc_config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[PC_CONFIG_SEED], &platform_core::ID)
}

pub fn role_pda(role: Role, member: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[PC_ROLE_SEED, &[role as u8], member.as_ref()],
        &platform_core::ID,
    )
}

/// Registers `game_program` in platform-core with `owner` and `status`.
pub fn put_game_entry(
    svm: &mut LiteSVM,
    game_program: &Pubkey,
    owner: &Pubkey,
    status: GameStatus,
) {
    let (address, bump) = game_entry_pda(game_program);
    let entry = GameEntry {
        version: 1,
        bump,
        status,
        game_program: *game_program,
        owner: *owner,
        game_authority: Pubkey::new_unique(),
        game_authority_bump: 255,
        registered_by: Pubkey::new_unique(),
        registered_at: T0,
        updated_at: T0,
        _reserved: [0; 64],
    };
    put_pc_account(svm, address, &entry);
}

/// Writes an arbitrary-owner copy of a GameEntry (for owner-check tests).
pub fn put_fake_game_entry(
    svm: &mut LiteSVM,
    game_program: &Pubkey,
    owner: &Pubkey,
    fake_owner: Pubkey,
) {
    put_game_entry(svm, game_program, owner, GameStatus::Active);
    let (address, _) = game_entry_pda(game_program);
    let mut acct = svm.get_account(&address).unwrap();
    acct.owner = fake_owner;
    svm.set_account(address, acct).unwrap();
}

pub fn put_pc_config(svm: &mut LiteSVM, ad_fee_bps: u16) {
    let (address, bump) = pc_config_pda();
    let cfg = PlatformConfig {
        version: 1,
        bump,
        boost_fee_bps: 100,
        ad_fee_bps,
        admin_count: 1,
        treasury_bump: Pubkey::find_program_address(&[PC_TREASURY_SEED], &platform_core::ID).1,
        initialized_at: T0,
        updated_at: T0,
        _reserved: [0; 64],
    };
    put_pc_account(svm, address, &cfg);
}

pub fn put_role(svm: &mut LiteSVM, role: Role, member: &Pubkey) {
    let (address, bump) = role_pda(role, member);
    let rm = RoleMember {
        version: 1,
        bump,
        role,
        member: *member,
        granted_by: Pubkey::new_unique(),
        granted_at: T0,
        _reserved: [0; 32],
    };
    put_pc_account(svm, address, &rm);
}

// --------------------------------------------------------- ad-pool PDAs

pub fn ad_config_pda(game_program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[AD_CONFIG_SEED, game_program.as_ref()], &ad_pool::id()).0
}

pub fn ad_slot_pda(game_program: &Pubkey, epoch: u64, slot_index: u8) -> Pubkey {
    Pubkey::find_program_address(
        &[
            AD_SLOT_SEED,
            game_program.as_ref(),
            &epoch.to_le_bytes(),
            &[slot_index],
        ],
        &ad_pool::id(),
    )
    .0
}

// -------------------------------------------------------- config builders

#[derive(Clone)]
pub struct ConfigArgs {
    pub genesis_ts: i64,
    pub epoch_duration_secs: i64,
    pub auction_close_lead_secs: i64,
    pub min_bid: u64,
    pub min_increment_bps: u16,
    pub bid_horizon_epochs: u16,
    pub slots: Vec<(u16, u16)>,
}

impl ConfigArgs {
    /// 1-day epochs, 7-day lead, 30-day horizon, 5% increment, two slots.
    pub fn default_with_floor(floor: u64) -> Self {
        Self {
            genesis_ts: T0,
            epoch_duration_secs: DAY,
            auction_close_lead_secs: 7 * DAY,
            min_bid: floor,
            min_increment_bps: 500,
            bid_horizon_epochs: 30,
            slots: vec![(728, 90), (300, 250)],
        }
    }
}

pub fn ix_init_ad_config(
    game_owner: &Pubkey,
    game_program: &Pubkey,
    a: &ConfigArgs,
) -> Instruction {
    ix_init_ad_config_with(game_owner, game_program, &game_entry_pda(game_program).0, a)
}

pub fn ix_init_ad_config_with(
    game_owner: &Pubkey,
    game_program: &Pubkey,
    game_entry: &Pubkey,
    a: &ConfigArgs,
) -> Instruction {
    Instruction::new_with_bytes(
        ad_pool::id(),
        &ad_pool::instruction::InitAdConfig {
            game_program: *game_program,
            genesis_ts: a.genesis_ts,
            epoch_duration_secs: a.epoch_duration_secs,
            auction_close_lead_secs: a.auction_close_lead_secs,
            min_bid: a.min_bid,
            min_increment_bps: a.min_increment_bps,
            bid_horizon_epochs: a.bid_horizon_epochs,
            slots: a
                .slots
                .iter()
                .map(|(w, h)| AdSlotSpecInput {
                    width: *w,
                    height: *h,
                })
                .collect(),
        }
        .data(),
        ad_pool::accounts::InitAdConfig {
            game_owner: *game_owner,
            game_entry: *game_entry,
            ad_config: ad_config_pda(game_program),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    )
}

fn manage_metas(
    game_owner: &Pubkey,
    game_program: &Pubkey,
) -> Vec<anchor_lang::prelude::AccountMeta> {
    ad_pool::accounts::ManageAdConfig {
        game_owner: *game_owner,
        game_entry: game_entry_pda(game_program).0,
        ad_config: ad_config_pda(game_program),
    }
    .to_account_metas(None)
}

pub fn ix_update_ad_config(
    game_owner: &Pubkey,
    game_program: &Pubkey,
    auction_close_lead_secs: Option<i64>,
    min_bid: Option<u64>,
    min_increment_bps: Option<u16>,
    bid_horizon_epochs: Option<u16>,
) -> Instruction {
    Instruction::new_with_bytes(
        ad_pool::id(),
        &ad_pool::instruction::UpdateAdConfig {
            auction_close_lead_secs,
            min_bid,
            min_increment_bps,
            bid_horizon_epochs,
        }
        .data(),
        manage_metas(game_owner, game_program),
    )
}

pub fn ix_add_slot(
    game_owner: &Pubkey,
    game_program: &Pubkey,
    width: u16,
    height: u16,
) -> Instruction {
    Instruction::new_with_bytes(
        ad_pool::id(),
        &ad_pool::instruction::AddSlot { width, height }.data(),
        manage_metas(game_owner, game_program),
    )
}

pub fn ix_set_slot_active(
    game_owner: &Pubkey,
    game_program: &Pubkey,
    slot_index: u8,
    active: bool,
) -> Instruction {
    Instruction::new_with_bytes(
        ad_pool::id(),
        &ad_pool::instruction::SetSlotActive { slot_index, active }.data(),
        manage_metas(game_owner, game_program),
    )
}

/// A game registered (Active) in platform-core with its owner funded.
pub struct Game {
    pub owner: Keypair,
    pub program: Pubkey,
}

pub fn new_game(svm: &mut LiteSVM) -> Game {
    let owner = funded(svm);
    let program = Pubkey::new_unique();
    put_game_entry(svm, &program, &owner.pubkey(), GameStatus::Active);
    Game { owner, program }
}

/// `new_game` + a valid default config.
pub fn new_configured_game(svm: &mut LiteSVM) -> (Game, ConfigArgs) {
    let g = new_game(svm);
    let a = ConfigArgs::default_with_floor(floor(svm));
    assert_ok(&send(
        svm,
        &[ix_init_ad_config(&g.owner.pubkey(), &g.program, &a)],
        &[&g.owner],
    ));
    (g, a)
}

// ------------------------------------------------------- auction builders

/// Epochs biddable at `now = T0` with the default config: start ∈ (T0 + 7 d, T0 + 30 d].
pub const FIRST_OPEN_EPOCH: u64 = 8;
pub const LAST_OPEN_EPOCH: u64 = 30;

pub fn ix_open_auction(
    bidder: &Pubkey,
    game_program: &Pubkey,
    epoch: u64,
    slot_index: u8,
    amount: u64,
) -> Instruction {
    Instruction::new_with_bytes(
        ad_pool::id(),
        &ad_pool::instruction::OpenAuction {
            game_program: *game_program,
            epoch,
            slot_index,
            amount,
        }
        .data(),
        ad_pool::accounts::OpenAuction {
            bidder: *bidder,
            game_entry: game_entry_pda(game_program).0,
            config: pc_config_pda().0,
            ad_config: ad_config_pda(game_program),
            auction: ad_slot_pda(game_program, epoch, slot_index),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    )
}

pub fn ix_bid_with(
    bidder: &Pubkey,
    auction: &Pubkey,
    game_entry: &Pubkey,
    prev_bidder: &Pubkey,
    amount: u64,
) -> Instruction {
    Instruction::new_with_bytes(
        ad_pool::id(),
        &ad_pool::instruction::Bid { amount }.data(),
        ad_pool::accounts::Bid {
            bidder: *bidder,
            auction: *auction,
            game_entry: *game_entry,
            prev_bidder: *prev_bidder,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    )
}

/// `bid` against the leader currently stored on chain.
pub fn ix_bid(
    svm: &LiteSVM,
    bidder: &Pubkey,
    game_program: &Pubkey,
    epoch: u64,
    slot_index: u8,
    amount: u64,
) -> Instruction {
    let auction = ad_slot_pda(game_program, epoch, slot_index);
    let leader = fetch::<ad_pool::state::AdSlotAuction>(svm, &auction).highest_bidder;
    ix_bid_with(
        bidder,
        &auction,
        &game_entry_pda(game_program).0,
        &leader,
        amount,
    )
}

pub fn auction_of(
    svm: &LiteSVM,
    game_program: &Pubkey,
    epoch: u64,
    slot_index: u8,
) -> ad_pool::state::AdSlotAuction {
    fetch(svm, &ad_slot_pda(game_program, epoch, slot_index))
}
