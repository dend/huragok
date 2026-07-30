//! GUObjectArray access - enumerate the global UObject table.

use crate::mem::base;
use crate::offsets::*;

/// Number of live object slots.
#[inline]
pub fn num_elements() -> i32 {
    unsafe { *((base() + GUOBJECTARRAY + UOA_NUMELEMENTS) as *const i32) }
}

/// Number of allocated chunks.
#[inline]
pub fn num_chunks() -> i32 {
    unsafe { *((base() + GUOBJECTARRAY + UOA_NUMCHUNKS) as *const i32) }
}

/// Resolve the UObject at global index `i` (null if empty / out of range).
pub fn object_at(i: i32) -> *mut u8 {
    if i < 0 {
        return core::ptr::null_mut();
    }
    unsafe {
        let gobj = base() + GUOBJECTARRAY;
        // Objects: *FUObjectItem[] - array of chunk pointers.
        let chunks = *((gobj + UOA_OBJECTS) as *const *const *mut u8);
        if chunks.is_null() {
            return core::ptr::null_mut();
        }
        let ci = (i as usize) / ELEMENTS_PER_CHUNK;
        if ci as i32 >= num_chunks() {
            return core::ptr::null_mut();
        }
        let chunk = *chunks.add(ci);
        if chunk.is_null() {
            return core::ptr::null_mut();
        }
        // FUObjectItem = { UObject* Object; ... }; first field is the object.
        let item = (chunk as usize) + ((i as usize) % ELEMENTS_PER_CHUNK) * ITEM_STRIDE;
        *(item as *const *mut u8)
    }
}
