//! TEST ONLY — `worker/src/image.rs` compiled for wasm32 with a C ABI for node (`run.mjs`).
//! A panic traps (`unreachable`) and node reports it. Uses the real file, not a copy.
#[path = "../../src/image.rs"]
pub mod image;

use core::cell::UnsafeCell;

struct Buf(UnsafeCell<[u8; 65536]>);
unsafe impl Sync for Buf {}
static BUF: Buf = Buf(UnsafeCell::new([0; 65536]));

#[unsafe(no_mangle)]
pub extern "C" fn buf_ptr() -> *mut u8 {
    BUF.0.get() as *mut u8
}

fn bytes(len: usize) -> &'static [u8] {
    unsafe { core::slice::from_raw_parts(BUF.0.get() as *const u8, len) }
}

/// 0 = Ok, 1 = Err.
#[unsafe(no_mangle)]
pub extern "C" fn sniff(len: usize) -> i32 {
    match image::sniff(bytes(len)) {
        Ok(_) => 0,
        Err(_) => 1,
    }
}

/// Mutate the loaded seed `iters` times (same xorshift as tests/intake.rs).
#[unsafe(no_mangle)]
pub extern "C" fn fuzz(len: usize, iters: u32) -> u32 {
    let seed: Vec<u8> = bytes(len).to_vec();
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..iters {
        let mut b = seed.clone();
        for _ in 0..4 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let j = (x as usize) % b.len();
            b[j] = (x >> 32) as u8;
        }
        x ^= x << 13;
        b.truncate(1 + (x as usize) % b.len());
        let _ = image::sniff(&b);
    }
    iters
}
