//! Decode an account fetched with curl (native). Example — devnet PlatformConfig:
//!   curl -s https://api.devnet.solana.com -H 'content-type: application/json' \
//!     -d '{"jsonrpc":"2.0","id":1,"method":"getAccountInfo","params":["Be8StYkiu7eBMb6m3TvG7QzMZpFL69bcn1kGmZwSmrJ7",{"encoding":"base64"}]}' \
//!   | cargo run -q --example decode -- platform_config
use std::io::Read;

fn main() {
    let kind = std::env::args().nth(1).expect(
        "kind: ad_slot_auction | game_ad_config | game_entry | role_member | platform_config",
    );
    let mut body = String::new();
    std::io::stdin().read_to_string(&mut body).unwrap();
    let info = ad_pool_worker::rpc::parse_account_info(&body)
        .unwrap()
        .expect("account not found");
    use ad_pool_worker::accounts::*;
    let out = match kind.as_str() {
        "ad_slot_auction" => {
            serde_json::to_string_pretty(&decode_ad_slot_auction(&info.data).unwrap())
        }
        "game_ad_config" => {
            serde_json::to_string_pretty(&decode_game_ad_config(&info.data).unwrap())
        }
        "game_entry" => serde_json::to_string_pretty(&decode_game_entry(&info.data).unwrap()),
        "role_member" => serde_json::to_string_pretty(&decode_role_member(&info.data).unwrap()),
        "platform_config" => {
            serde_json::to_string_pretty(&decode_platform_config(&info.data).unwrap())
        }
        k => panic!("unknown kind {k}"),
    }
    .unwrap();
    println!("owner: {}\nlamports: {}\n{out}", info.owner, info.lamports);
}
