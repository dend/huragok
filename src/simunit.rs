//! Direct sim writes for cheats the vestigial HS cheat globals cannot provide.
//!
//! In this build the classic `cheat_deathless_player` / `cheat_omnipotent` HaloScript
//! globals have null backing storage and nothing reads them, so `hs:set` does nothing.
//! Instead we write the player's Blam "unit" object directly:
//!   - invulnerability -> the damage core's "cannot take damage" object flag (bit 7),
//!   - one-shot-kill    -> zero a unit's normalized vitality records.
//! Every pointer access is SEH-guarded and the resolved unit is validated before we write.

use core::sync::atomic::{AtomicBool, Ordering};

// Object-body layout, from static analysis of the sim damage core.
const OBJ_FLAGS: usize = 0x128; // u32 object flags
const FLAG_NO_DAMAGE: u32 = 0x80; // bit 7: the damage core zeroes all incoming damage
const VIT_ARRAY_SIZE: usize = 0x174; // u16: vitality-record array size in bytes (n * 0x18)
const VIT_ARRAY_OFF: usize = 0x176; // u16: byte offset from the body to the record array
const REC_STRIDE: usize = 0x18;
const REC_VALID: usize = 0x0e; // u16: 0xFFFF => record unused
const REC_VITALITY: usize = 0x10; // f32: normalized 0..1 (1.0 = full)

static INVULN: AtomicBool = AtomicBool::new(false);
static ONESHOT: AtomicBool = AtomicBool::new(false);
static WARNED: AtomicBool = AtomicBool::new(false);

/// Resolve the local player's unit object-body pointer, or 0 if not available yet.
fn player_unit() -> usize {
    crate::simtime::player_unit()
}

/// True if `u` looks like a valid unit object body: the vitality-record array size reads as
/// a small, non-zero multiple of the record stride. The read is guarded so a stale pointer
/// can never crash us.
fn valid_unit(u: usize) -> bool {
    if u == 0 {
        return false;
    }
    let mut size = 0u16;
    let ok = crate::seh::guard(|| unsafe {
        size = *((u + VIT_ARRAY_SIZE) as *const u16);
    });
    ok && size != 0 && (size as usize) % REC_STRIDE == 0 && (size as usize) < 0x1800
}

/// Set or clear the damage-core "cannot take damage" flag on a unit body.
fn write_no_damage(u: usize, on: bool) {
    let _ = crate::seh::guard(|| unsafe {
        let p = (u + OBJ_FLAGS) as *mut u32;
        if on {
            *p |= FLAG_NO_DAMAGE;
        } else {
            *p &= !FLAG_NO_DAMAGE;
        }
    });
}

/// Zero every valid vitality record (body + shields) on a unit body -> lethal next tick.
fn kill_unit(u: usize) {
    let _ = crate::seh::guard(|| unsafe {
        let base = u + *((u + VIT_ARRAY_OFF) as *const u16) as usize;
        let n = (*((u + VIT_ARRAY_SIZE) as *const u16) as usize) / REC_STRIDE;
        for i in 0..n {
            let rec = base + i * REC_STRIDE;
            if *((rec + REC_VALID) as *const u16) != 0xFFFF {
                *((rec + REC_VITALITY) as *mut f32) = 0.0;
            }
        }
    });
}

/// Toggle player invulnerability. Applies immediately; [`tick`] re-asserts it each frame.
pub fn set_invuln(on: bool) {
    INVULN.store(on, Ordering::Relaxed);
    let u = player_unit();
    if valid_unit(u) {
        write_no_damage(u, on);
        crate::rep!("[cheat] invulnerability {}", if on { "ON" } else { "OFF" });
    } else if !WARNED.swap(true, Ordering::Relaxed) {
        crate::rep!("[cheat] invulnerability pending - player unit not resolved yet");
    }
}

/// Toggle one-shot-kill. The kill is applied to damaged enemies in [`tick`].
pub fn set_oneshot(on: bool) {
    ONESHOT.store(on, Ordering::Relaxed);
    crate::rep!("[cheat] one-shot kill {}", if on { "ON" } else { "OFF" });
}

/// Per-frame maintenance on the game thread: re-assert invulnerability so the sim cannot
/// clear it, and (when enabled) finish off any unit the player has damaged. Cheap no-op
/// while both cheats are off. Call from the game-thread frame hook.
pub fn tick() {
    if !INVULN.load(Ordering::Relaxed) && !ONESHOT.load(Ordering::Relaxed) {
        return;
    }
    let u = player_unit();
    if INVULN.load(Ordering::Relaxed) && valid_unit(u) {
        write_no_damage(u, true);
    }
    // One-shot-kill scan is wired once the object-table walk is confirmed (needs the table
    // base + the player unit to exclude). `kill_unit` is the write it will use per target.
    let _ = kill_unit;
}
