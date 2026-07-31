//! Module-base resolution and raw memory patching.
//!
//! Everything the engine touches is addressed as `base + RVA`, where `base` is the
//! game module's load address (ASLR-safe, resolved once via `GetModuleHandleW(NULL)`).

use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{VirtualProtect, PAGE_EXECUTE_READWRITE};

static BASE: AtomicUsize = AtomicUsize::new(0);
static SIM_BASE: AtomicUsize = AtomicUsize::new(0);

/// Resolve and cache the game module base. Call once, early.
pub fn init() {
    let h = unsafe { GetModuleHandleW(core::ptr::null()) };
    BASE.store(h as usize, Ordering::SeqCst);
}

/// The cached module base (0 until [`init`] runs).
#[inline]
pub fn base() -> usize {
    BASE.load(Ordering::Relaxed)
}

/// Base address of the Blam simulation module, or 0 if not loaded.
/// Cached after first successful resolve. The sim clock (`game_time_globals`) lives here.
pub fn sim_base() -> usize {
    let cached = SIM_BASE.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    // UTF-16, NUL-terminated name of the simulation module.
    let name: Vec<u16> = "HaloSimulation_tag_release.dll"
        .encode_utf16()
        .chain(core::iter::once(0))
        .collect();
    let h = unsafe { GetModuleHandleW(name.as_ptr()) } as usize;
    if h != 0 {
        SIM_BASE.store(h, Ordering::Relaxed);
    }
    h
}

/// Absolute address of a module RVA.
#[inline]
pub fn at(rva: usize) -> usize {
    base() + rva
}

/// Typed pointer to a module RVA.
#[inline]
pub fn ptr<T>(rva: usize) -> *mut T {
    (base() + rva) as *mut T
}

/// Transmute a module RVA into a callable function pointer of type `F`.
///
/// # Safety
/// `F` must exactly match the native calling convention and signature at `rva`.
#[inline]
pub unsafe fn func<F: Copy>(rva: usize) -> F {
    let addr = base() + rva;
    // SAFETY: caller guarantees the signature; F is a fn-pointer-sized value.
    core::mem::transmute_copy::<usize, F>(&addr)
}

/// Overwrite a pointer-sized slot (vtable entry, `.rdata` dispatch pointer, ...)
/// after making its page writable. Returns the previous value.
///
/// # Safety
/// `slot` must point at a valid, aligned, pointer-sized location.
pub unsafe fn patch_ptr(slot: *mut usize, new: usize) -> usize {
    let mut old_prot = 0u32;
    let size = core::mem::size_of::<usize>();
    VirtualProtect(slot as *const c_void, size, PAGE_EXECUTE_READWRITE, &mut old_prot);
    let prev = *slot;
    *slot = new;
    VirtualProtect(slot as *const c_void, size, old_prot, &mut old_prot);
    prev
}
