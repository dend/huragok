//! Live game stats, read via reflection on the game thread and shown by the panel.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::offsets::{RF_ARCHETYPE_OBJECT, RF_BIT30, RF_CLASS_DEFAULT_OBJECT, UO_FLAGS};
use crate::pawn::{call_ret_bool, call_ret_f32, call_ret_ptr, get_component, get_pawn};
use crate::ue::fname::obj_name;
use crate::ue::object::{num_elements, object_at};
use crate::ue::reflect::{class_of, find_class, is_a};

pub struct Stats {
    pub health: f32, // 0..100, NaN until read
    pub shield: f32,
    pub enemies_alive: i32, // -1 until counted
    pub enemies_total: i32,
    pub valid: bool,
}

static STATS: Mutex<Stats> = Mutex::new(Stats {
    health: f32::NAN,
    shield: f32::NAN,
    enemies_alive: -1,
    enemies_total: -1,
    valid: false,
});

static C_OBJ_ACTOR: AtomicUsize = AtomicUsize::new(0);
static C_OBJ_COMP: AtomicUsize = AtomicUsize::new(0);
static REFRESH_N: AtomicUsize = AtomicUsize::new(0);
static DIAG: AtomicBool = AtomicBool::new(false);

fn lock() -> std::sync::MutexGuard<'static, Stats> {
    STATS.lock().unwrap_or_else(|e| e.into_inner())
}

fn cached(slot: &AtomicUsize, resolve: impl FnOnce() -> *mut u8) -> *mut u8 {
    let cur = slot.load(Ordering::Relaxed);
    if cur != 0 {
        return cur as *mut u8;
    }
    let p = resolve();
    slot.store(p as usize, Ordering::Relaxed);
    p
}

/// Values for the UI: (health, shield, enemies_alive, enemies_total, valid).
pub fn snapshot() -> (f32, f32, i32, i32, bool) {
    let s = lock();
    (s.health, s.shield, s.enemies_alive, s.enemies_total, s.valid)
}

/// Refresh from the game (game thread only). Health/shield every call, enemy count throttled.
pub fn refresh(pc: *mut u8) {
    crate::seh::guard(|| unsafe {
        let pawn = get_pawn(pc);
        if pawn.is_null() {
            return;
        }
        let obj_actor = call_ret_ptr(pawn, "GetBlamObjectActor");
        if obj_actor.is_null() {
            return;
        }
        let objc = cached(&C_OBJ_COMP, || find_class("BlamObjectComponent"));
        let oc = get_component(obj_actor, objc);
        if !oc.is_null() {
            let h = call_ret_f32(oc, "GetBodyVitality") * 100.0;
            let s = call_ret_f32(oc, "GetShieldVitality") * 100.0;
            let mut st = lock();
            st.health = h;
            st.shield = s;
            st.valid = true;
        }

        if REFRESH_N.fetch_add(1, Ordering::Relaxed) % 16 == 0 {
            let (alive, total) = count_enemies();
            let mut st = lock();
            st.enemies_alive = alive;
            st.enemies_total = total;
        }
    });
}

/// Sweep GUObjectArray for enemy characters. The team predicates read 0 for every
/// actor on this build, so identify enemies by species class name; alive = not IsDead.
unsafe fn count_enemies() -> (i32, i32) {
    let boa = cached(&C_OBJ_ACTOR, || find_class("BlamObjectActor"));
    let objc = cached(&C_OBJ_COMP, || find_class("BlamObjectComponent"));
    if boa.is_null() {
        return (-1, -1);
    }
    const ENEMY: &[&str] = &[
        "Grunt", "Elite", "Jackal", "Hunter", "Brute", "Drone", "Flood", "Infection", "Combat",
        "Carrier", "Sentinel", "Sangheili", "Kig", "Unggoy", "Jiralhanae",
    ];
    let n = num_elements();
    let mut alive = 0;
    let mut total = 0;
    let mut sample = String::new();
    for i in 0..n {
        let o = object_at(i);
        if o.is_null() {
            continue;
        }
        let flags = *((o as usize + UO_FLAGS) as *const u32);
        if flags & (RF_CLASS_DEFAULT_OBJECT | RF_ARCHETYPE_OBJECT | RF_BIT30) != 0 {
            continue;
        }
        if !is_a(class_of(o), boa) {
            continue;
        }
        let cname = obj_name(class_of(o));
        if !cname.contains("BipedActor") {
            continue; // characters only
        }
        if sample.len() < 300 {
            sample.push_str(&cname);
            sample.push(' ');
        }
        if !ENEMY.iter().any(|e| cname.contains(e)) {
            continue; // ally / neutral
        }
        total += 1;
        let oc = get_component(o, objc);
        if !oc.is_null() && !call_ret_bool(oc, "IsDead") {
            alive += 1;
        }
    }
    if !DIAG.swap(true, Ordering::Relaxed) {
        crate::rep!("[stats] enemy sweep: {total} enemies, {alive} alive");
        crate::rep!("[stats] biped species seen: {sample}");
    }
    (alive, total)
}
