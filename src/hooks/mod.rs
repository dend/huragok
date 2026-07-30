//! Hook installation primitives.

pub mod camera;
pub mod imgui;
pub mod pc;

use core::ffi::c_void;
use windows_sys::Win32::System::Memory::{VirtualProtect, PAGE_READWRITE};

/// Overwrite vtable `slot` of the object at `obj` with `detour`, returning the
/// original entry (so the detour can tail-call it). Affects every instance whose
/// class shares this vtable.
///
/// # Safety
/// `obj` must be a valid object whose first field is a vtable pointer, and `slot`
/// must be in range. `detour` must be an `extern "system"` fn matching the slot.
pub unsafe fn hook_vtable_slot(obj: *mut u8, slot: usize, detour: usize) -> usize {
    let vt = *(obj as *const *mut usize); // vtable pointer
    let entry = vt.add(slot);
    let size = core::mem::size_of::<usize>();
    let mut old = 0u32;
    VirtualProtect(entry as *const c_void, size, PAGE_READWRITE, &mut old);
    let orig = *entry;
    *entry = detour;
    VirtualProtect(entry as *const c_void, size, old, &mut old);
    orig
}
