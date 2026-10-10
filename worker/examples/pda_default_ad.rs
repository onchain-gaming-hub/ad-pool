//! Dev helper: print the platform-core DefaultAd PDA of a size.
//!   cargo run -q --example pda_default_ad -- <width> <height>
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pc = "AJnPSYsn7J2muHfKztRtpZz5kBP94dWkNZLr9aMhu18G"
        .parse()
        .unwrap();
    println!(
        "{}",
        ad_pool_worker::pda::default_ad(&pc, a[1].parse().unwrap(), a[2].parse().unwrap())
    );
}
