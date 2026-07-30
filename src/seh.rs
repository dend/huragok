//! Structured-exception guard - the Rust side of `csrc/seh.c`.
//!
//! Any code that dereferences a raw game pointer at an offset we RE'd can fault if
//! the offset moved in a patch. Wrap those calls in [`guard`] so a fault is caught,
//! logged, and the game keeps running - mirroring the C++ engine's `__try/__except`.

use core::ffi::c_void;

extern "C" {
    fn huragok_seh_try(cb: extern "C" fn(*mut c_void), ctx: *mut c_void) -> i32;
}

/// Run `f` under a structured-exception frame.
/// Returns `true` if it completed, `false` if it faulted (e.g. access violation).
///
/// `f` must not unwind; the crate builds with `panic = "abort"`, so it won't.
pub fn guard<F: FnMut()>(mut f: F) -> bool {
    extern "C" fn trampoline<F: FnMut()>(ctx: *mut c_void) {
        // SAFETY: `ctx` is the `&mut F` handed to `huragok_seh_try` below, called once.
        let f = unsafe { &mut *(ctx as *mut F) };
        f();
    }
    unsafe { huragok_seh_try(trampoline::<F>, &mut f as *mut F as *mut c_void) != 0 }
}
