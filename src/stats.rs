//! Live game stats, read via reflection on the game thread and displayed by the panel.
//!
//! Chain (from the RE plan): pawn -> GetBlamObjectActor -> GetComponentByClass(
//! BlamObjectComponent) -> GetBodyVitality / GetShieldVitality.

use std::sync::Mutex;

pub struct Stats {
    pub health: f32, // 0..100, NaN until read
    pub shield: f32,
    pub valid: bool,
}

static STATS: Mutex<Stats> = Mutex::new(Stats {
    health: f32::NAN,
    shield: f32::NAN,
    valid: false,
});

fn lock() -> std::sync::MutexGuard<'static, Stats> {
    STATS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Current values for the UI: (health, shield, valid).
pub fn snapshot() -> (f32, f32, bool) {
    let s = lock();
    (s.health, s.shield, s.valid)
}

/// Refresh from the game (game thread only, via the PlayerController detour).
pub fn refresh(pc: *mut u8) {
    crate::seh::guard(|| unsafe {
        let pawn = crate::pawn::get_pawn(pc);
        if pawn.is_null() {
            return;
        }
        let obj_actor = crate::pawn::call_ret_ptr(pawn, "GetBlamObjectActor");
        if obj_actor.is_null() {
            return;
        }
        let boc = crate::ue::reflect::find_class("BlamObjectComponent");
        let oc = crate::pawn::get_component(obj_actor, boc);
        if oc.is_null() {
            return;
        }
        let h = crate::pawn::call_ret_f32(oc, "GetBodyVitality") * 100.0;
        let s = crate::pawn::call_ret_f32(oc, "GetShieldVitality") * 100.0;
        let mut st = lock();
        st.health = h;
        st.shield = s;
        st.valid = true;
    });
}
