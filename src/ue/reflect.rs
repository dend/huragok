//! Class-chain walking, function lookup, and well-known object discovery.

use super::fname::obj_name;
use super::object::{num_elements, object_at};
use crate::offsets::*;

/// The UClass of an object.
///
/// # Safety
/// `obj` must be null or a valid UObject.
pub unsafe fn class_of(obj: *const u8) -> *mut u8 {
    if obj.is_null() {
        return core::ptr::null_mut();
    }
    *((obj as usize + UO_CLASS) as *const *mut u8)
}

/// The SuperStruct of a UStruct/UClass.
///
/// # Safety
/// `cls` must be null or a valid UStruct.
pub unsafe fn super_of(cls: *const u8) -> *mut u8 {
    if cls.is_null() {
        return core::ptr::null_mut();
    }
    *((cls as usize + US_SUPER) as *const *mut u8)
}

/// True if `cls` is `target` or descends from it (pointer walk, no name decode).
pub fn is_a(mut cls: *mut u8, target: *mut u8) -> bool {
    if target.is_null() {
        return false;
    }
    unsafe {
        for _ in 0..64 {
            if cls.is_null() {
                return false;
            }
            if cls == target {
                return true;
            }
            cls = super_of(cls);
        }
    }
    false
}

/// Log every UFunction whose name contains one of `needles` (Owner::Name ParmsSize).
/// One-shot diagnostic for finding a callable function by keyword.
pub fn probe_functions(needles: &[&str]) {
    let n = num_elements();
    let mut count = 0;
    for i in 0..n {
        let o = object_at(i);
        if o.is_null() {
            continue;
        }
        unsafe {
            if obj_name(class_of(o)) != "Function" {
                continue;
            }
            let name = obj_name(o);
            if !needles.iter().any(|s| name.contains(s)) {
                continue;
            }
            let outer = *((o as usize + crate::offsets::UO_OUTER) as *const *mut u8);
            let ps = *((o as usize + crate::offsets::UFN_PARMSSIZE) as *const u16);
            crate::rep!("[func] {}::{} ParmsSize={}", obj_name(outer), name, ps);
            count += 1;
            if count > 80 {
                break;
            }
        }
    }
    crate::rep!("[func] {} matches", count);
}

/// Find a class default object by exact name (`Default__<ClassName>`).
pub fn find_cdo(default_name: &str) -> *mut u8 {
    let n = num_elements();
    for i in 0..n {
        let o = object_at(i);
        if o.is_null() {
            continue;
        }
        if unsafe { obj_name(o) } == default_name {
            return o;
        }
    }
    core::ptr::null_mut()
}

/// True if any class in `cls`'s chain has a name containing `want`.
pub fn class_chain_has(mut cls: *mut u8, want: &str) -> bool {
    unsafe {
        for _ in 0..64 {
            if cls.is_null() {
                break;
            }
            if obj_name(cls).contains(want) {
                return true;
            }
            cls = super_of(cls);
        }
    }
    false
}

/// Find a UFunction named `fname` on `cls` or any superclass (null if absent).
pub fn find_function(mut cls: *mut u8, fname: &str) -> *mut u8 {
    unsafe {
        for _ in 0..64 {
            if cls.is_null() {
                break;
            }
            let mut child = *((cls as usize + US_CHILDREN) as *const *mut u8);
            let mut steps = 0;
            while !child.is_null() && steps < 8192 {
                if obj_name(class_of(child)) == "Function" && obj_name(child) == fname {
                    return child;
                }
                child = *((child as usize + UF_NEXT) as *const *mut u8);
                steps += 1;
            }
            cls = super_of(cls);
        }
    }
    core::ptr::null_mut()
}

/// Find a live (non-CDO, non-archetype) UClass whose name contains `want`.
pub fn find_class(want: &str) -> *mut u8 {
    let n = num_elements();
    for i in 0..n {
        let o = object_at(i);
        if o.is_null() {
            continue;
        }
        unsafe {
            if obj_name(class_of(o)) == "Class" {
                let name = obj_name(o);
                if name.contains(want) {
                    return o;
                }
            }
        }
    }
    core::ptr::null_mut()
}

/// Find a live (non-CDO, non-archetype, non-template) object whose class chain
/// contains a class named like `class_name`. Null until such an object exists.
pub fn find_live_by_class(class_name: &str) -> *mut u8 {
    let n = num_elements();
    for i in 0..n {
        let o = object_at(i);
        if o.is_null() {
            continue;
        }
        unsafe {
            let flags = *((o as usize + UO_FLAGS) as *const u32);
            if flags & (RF_CLASS_DEFAULT_OBJECT | RF_ARCHETYPE_OBJECT | RF_BIT30) != 0 {
                continue;
            }
            if !class_chain_has(class_of(o), class_name) {
                continue;
            }
            let name = obj_name(o);
            if name.starts_with("Default__") || name.contains("_GEN_VARIABLE") {
                continue;
            }
            return o;
        }
    }
    core::ptr::null_mut()
}

/// Scan the object table for the live MeteoritePlayerController (null until it exists).
pub fn find_player_controller() -> *mut u8 {
    let n = num_elements();
    for i in 0..n {
        let o = object_at(i);
        if o.is_null() {
            continue;
        }
        unsafe {
            let flags = *((o as usize + UO_FLAGS) as *const u32);
            if flags & (RF_CLASS_DEFAULT_OBJECT | RF_ARCHETYPE_OBJECT | RF_BIT30) != 0 {
                continue;
            }
            // Match INSTANCES via the class chain, not the object name. Matching the
            // name also hits the UClass object "MeteoritePlayerController" that exists
            // from startup, which made us proceed before gameplay (and its camera) existed.
            if !class_chain_has(class_of(o), "MeteoritePlayerController") {
                continue;
            }
            let name = obj_name(o);
            if name.starts_with("Default__") || name.contains("_GEN_VARIABLE") {
                continue;
            }
            return o;
        }
    }
    core::ptr::null_mut()
}
