//! Module-base resolution and raw memory patching.
//!
//! Everything the engine touches is addressed as `base + RVA`, where `base` is the
//! game module's load address (ASLR-safe, resolved once via `GetModuleHandleW(NULL)`).

use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{VirtualProtect, PAGE_EXECUTE_READWRITE};

static BASE: AtomicUsize = AtomicUsize::new(0);

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
