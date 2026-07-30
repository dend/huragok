//! Unreal Engine reflection: FName, GUObjectArray, class/function lookup, ProcessEvent.

pub mod fname;
pub mod object;
pub mod process_event;
pub mod reflect;

/// Reproduce the C++ `verify`: decode name id 0 (must be `"None"`), count objects,
/// and report whether a PlayerController resolves yet. Returns true once the world
/// is up enough to trust reflection.
pub fn verify() -> bool {
    let name0 = fname::name_by_id(0);
    let num = object::num_elements();
    let pc = !reflect::find_player_controller().is_null();
    crate::rep!(
        "[verify] name(0)=\"{name0}\"  objects={num}  PC={}",
        if pc { "yes" } else { "no" }
    );
    name0 == "None" && num > 0
}
