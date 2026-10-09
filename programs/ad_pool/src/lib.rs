//! ad-pool — per-game ad slot auctions in SOL, one auction per (game, epoch, slot),
//! with creatives written on chain by a Moderator-role Worker after off-chain checks.
//! Design: specs/designs/01-program-design-v1.md · Plan: specs/designs/02-implementation-plan.md
//!
//! Phase P0: layouts, constants, errors, events and pure helpers only (no instructions yet).

pub mod constants;
pub mod error;
pub mod events;
pub mod math;
pub mod state;

use anchor_lang::prelude::*;

pub use constants::*;
pub use error::*;
pub use events::*;
pub use state::*;

declare_id!("6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk");

// platform-core is read through its IDL only (idls/platform_core.json) — no crate
// dependency (pc 03 §0). Types generated here have no `PartialEq`: use `matches!`.
declare_program!(platform_core);

#[program]
pub mod ad_pool {}
