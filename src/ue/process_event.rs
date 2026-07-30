//! Calling UFunctions through `UObject::ProcessEvent` (vtable slot 79).

use core::ffi::c_void;

use super::reflect::{class_of, find_function};
use crate::offsets::*;

type PeFn = unsafe extern "system" fn(*mut u8, *mut u8, *mut c_void);

/// Invoke `UObject::ProcessEvent(func, parms)` on `obj` via its vtable.
///
/// # Safety
/// `obj`/`func` must be valid, and `parms` must match `func`'s parameter block.
pub unsafe fn process_event(obj: *mut u8, func: *mut u8, parms: *mut c_void) {
    if obj.is_null() || func.is_null() {
        return;
    }
    let vtbl = *(obj as *const *const usize);
    if vtbl.is_null() {
        return;
    }
    let slot = *vtbl.add(PROCESSEVENT_SLOT);
    let pe: PeFn = core::mem::transmute(slot);
    pe(obj, func, parms);
}

/// Resolve `fname` on `obj`'s class, optionally check `ParmsSize`, and ProcessEvent it.
///
/// `expect_ps < 0` skips the size check. Returns whether the call was made.
pub fn pe_call(obj: *mut u8, fname: &str, parms: *mut c_void, expect_ps: i32) -> bool {
    unsafe {
        if obj.is_null() {
            return false;
        }
        let f = find_function(class_of(obj), fname);
        if f.is_null() {
            crate::rep!("[pawn] {fname} not found");
            return false;
        }
        if expect_ps >= 0 {
            let ps = *((f as usize + UFN_PARMSSIZE) as *const u16) as i32;
            if ps != expect_ps {
                crate::rep!("[pawn] {fname} ParmsSize {ps} != {expect_ps} - skipped");
                return false;
            }
        }
        process_event(obj, f, parms);
        true
    }
}
