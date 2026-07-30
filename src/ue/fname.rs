//! FName decoding via the FNamePool.

use crate::mem::base;
use crate::offsets::*;

/// Decode an FName id into a `String` (empty on any inconsistency).
pub fn name_by_id(id: u32) -> String {
    unsafe {
        let pool = base() + FNAMEPOOL;
        let blk = (id >> NAME_BLOCK_BITS) as usize;
        let off = (id & ((1u32 << NAME_BLOCK_BITS) - 1)) as usize;

        let blocks = (pool + NP_BLOCKS) as *const *const u8;
        let block = *blocks.add(blk);
        if block.is_null() {
            return String::new();
        }
        let entry = block.add(off * 2);
        let header = *(entry as *const u16);
        let len = (header >> 6) as usize;
        let wide = (header & 1) != 0;
        if len == 0 || len > 1024 {
            return String::new();
        }
        let data = entry.add(2);
        if wide {
            let w = core::slice::from_raw_parts(data as *const u16, len);
            String::from_utf16_lossy(w)
        } else {
            let b = core::slice::from_raw_parts(data, len);
            String::from_utf8_lossy(b).into_owned()
        }
    }
}

/// Read a UObject's FName into a `String`.
///
/// # Safety
/// `obj` must be null or a valid UObject pointer.
pub unsafe fn obj_name(obj: *const u8) -> String {
    if obj.is_null() {
        return String::new();
    }
    let id = *((obj as usize + UO_NAME) as *const u32);
    name_by_id(id)
}
