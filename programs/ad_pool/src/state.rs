//! Account layouts — design §2/§3. Rules (same as platform-core 01 §0):
//! - byte 8 = `version`, byte 9 = `bump`
//! - fields that may be `memcmp`'d sit at fixed offsets near the head
//! - `_reserved` is carved for future fields; never reorder or rename a struct

pub mod ad_creative;
pub mod ad_slot_auction;
pub mod enums;
pub mod game_ad_config;

pub use ad_creative::*;
pub use ad_slot_auction::*;
pub use enums::*;
pub use game_ad_config::*;
