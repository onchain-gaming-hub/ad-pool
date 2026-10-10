//! Accepts every instruction and returns success (0). No accounts read, no state.
#![no_std]

#[no_mangle]
pub extern "C" fn entrypoint(_input: *mut u8) -> u64 {
    0
}

#[cfg(target_os = "solana")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
