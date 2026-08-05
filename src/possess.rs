//! Play-as-AI, Tier 1: lock the camera to an AI biped's eyes ("see through their eyes").
//!
//! Enemy/ally bipeds are `BlamObjectActor`-derived actors, not pawns, so UE `Possess` is
//! not the mechanism (see docs/research/play-as-ai.md). Instead we ride the existing camera
//! detour: pick a target biped, and each frame override `BlueprintUpdateCamera`'s POV with
//! the target's eye location + facing. Selection uses the free-cam aim ray if it is active,
//! otherwise the nearest biped to the player pawn.

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicI8, AtomicI32, AtomicU8, AtomicU32, AtomicUsize, Ordering};

use crate::offsets::{RF_ARCHETYPE_OBJECT, RF_BIT30, RF_CLASS_DEFAULT_OBJECT, UO_FLAGS};
use crate::pawn::{call_ret_bool, get_component, get_pawn};
use crate::ue::fname::obj_name;
use crate::ue::object::{num_elements, object_at};
use crate::ue::process_event::process_event;
use crate::ue::reflect::{class_of, find_class, find_function, is_a, list_properties, property_offset};

/// Component mirror offsets (EXE static analysis): normalized vitality floats.
const COMP_BODY_VIT: usize = 0x218;
const COMP_SHIELD_VIT: usize = 0x228;

// Deferred AI enable/disable so the hs command always runs on the safe game-thread drain
// (never from the camera detour). 1 = disable pending, 2 = enable pending, 0 = none.
static AI_PENDING: AtomicI8 = AtomicI8::new(0);

// Puppet horizontal move speed (Blam world units/sec; ~305 UE-uu per wu). Tune in-game.
const SPEED_WU: f32 = 2.2;

// Tracks whether ImGui has the cursor (toggled by ImGui.ToggleInput / Ctrl+I). While true the
// mod must not hijack the mouse for look, and clicks must not fire - the cursor belongs to the UI.
static CURSOR_ACTIVE: AtomicBool = AtomicBool::new(false);

/// True while ImGui input/cursor mode is active (UI has the mouse).
pub fn cursor_active() -> bool {
    CURSOR_ACTIVE.load(Ordering::Relaxed)
}

/// Flip the tracked cursor state - call whenever ImGui.ToggleInput runs.
pub fn note_cursor_toggle() {
    CURSOR_ACTIVE.fetch_xor(true, Ordering::Relaxed);
}

/// How far above the actor origin the eyes sit (world units).
const EYE_UP: f64 = 65.0;
/// First-person: nudge the camera this far forward of the eyes so it clears the head mesh.
const FP_FORWARD: f64 = 24.0;
/// Third-person boom: distance behind, height above the eyes, and downward look angle.
const TP_BACK: f64 = 200.0;
const TP_UP: f64 = 70.0;
const TP_PITCH: f64 = -12.0;

const VIEW_FIRST: u8 = 0;
const VIEW_THIRD: u8 = 1;
static VIEW: AtomicU8 = AtomicU8::new(VIEW_THIRD); // default third-person (not inside the mesh)

static FOLLOW: AtomicBool = AtomicBool::new(false);
static TARGET: AtomicUsize = AtomicUsize::new(0); // target actor pointer
static TARGET_IDX: AtomicI32 = AtomicI32::new(-1); // its GUObjectArray index (revalidation)
static F_GET_LOC: AtomicUsize = AtomicUsize::new(0); // cached AActor::K2_GetActorLocation
static F_GET_ROT: AtomicUsize = AtomicUsize::new(0); // cached AActor::K2_GetActorRotation
static BOA: AtomicUsize = AtomicUsize::new(0); // cached BlamObjectActor class
static OBJC: AtomicUsize = AtomicUsize::new(0); // cached BlamObjectComponent class

// Tier-3 control: when active, the local player's sim unit handle has been swapped to the
// target's, so game input drives the possessed unit. SAVED_HANDLE restores it.
static CONTROL: AtomicBool = AtomicBool::new(false);
static CUR_HANDLE: AtomicU32 = AtomicU32::new(0xffff_ffff); // possessed unit handle
static BRAINDEAD_SAVED: AtomicI32 = AtomicI32::new(1); // AI actor state saved before braindead
static SAVED_PLAYER_HANDLE: AtomicU32 = AtomicU32::new(0xffff_ffff); // player's real unit (restore)
static SAVED_CONTROL_HANDLE: AtomicU32 = AtomicU32::new(0xffff_ffff); // control-element unit (restore)
static SAVED_AI_DATUM: AtomicU32 = AtomicU32::new(0xffff_ffff); // unit's AI datum before detach (restore)
static SAVED_PLAYER_TEAM: AtomicI32 = AtomicI32::new(i32::MIN); // player team word before possession (restore)
static PAWN_PTR: AtomicUsize = AtomicUsize::new(0); // cached pawn (avoid get_pawn ProcessEvent per tick)
static POSSESSED_TEAM: AtomicU32 = AtomicU32::new(0); // possessed unit's original team word (re-assert)
static ANCHOR_MOVED: AtomicU32 = AtomicU32::new(0); // f32 bits: last anchor-follow jump distance (diag)

/// True while the camera should be locked to a target (read lock-free by the cam detour).
pub fn follow_active() -> bool {
    FOLLOW.load(Ordering::Relaxed)
}

/// HaloScript team name for the possessed unit's actual team index (unit body+0x1BA low byte) -
/// so allegiance/faction uses whatever we possessed (Covenant, Flood, Sentinel...), not a hardcoded
/// guess. None when not controlling a unit.
pub fn possessed_team_name() -> Option<&'static str> {
    if !CONTROL.load(Ordering::Relaxed) {
        return None;
    }
    // Use the ORIGINAL team stored on possess, NOT the live unit+0x1BA - the +0x1BC sync overwrites
    // the live value to the player's team (1), which made the toggle run "ai_allegiance player player".
    let w = POSSESSED_TEAM.load(Ordering::Relaxed);
    let team = if w != 0 {
        (w & 0xff) as u8
    } else {
        let h = CUR_HANDLE.load(Ordering::Relaxed);
        if h == 0xffff_ffff {
            return None;
        }
        (crate::simtime::unit_team(h)? & 0xff) as u8
    };
    Some(match team {
        0 => "default",
        1 => "player",
        2 => "human",
        3 => "covenant",
        4 => "flood",
        5 => "sentinel",
        6 => "unused6",
        7 => "unused7",
        8 => "unused8",
        _ => "unused9",
    })
}

/// True while we are NATIVELY driving a possessed unit (bind-slot control), as opposed to plain
/// spectate-follow. In this mode the mouse belongs to the game (native aim turns the body) and the
/// camera trails the game's own POV - see [`control_pov`] and `input::poll_possess_look`.
pub fn control_active() -> bool {
    CONTROL.load(Ordering::Relaxed)
}

// Third-person boom for native control, in UE units/degrees (the game POV is UE-space).
const TP_BACK_UU: f64 = 350.0; // camera distance behind the native POV
const TP_UP_UU: f64 = 85.0; // camera height above the native POV

/// Third-person boom that TRAILS the game's own first-person POV. The native POV sits at the
/// possessed unit's eye, oriented along the live control/aim rotation (mouse-driven, since native
/// control now owns the mouse). Pulling it straight back yields a camera that (a) follows the body
/// as the mouse turns it, (b) makes WASD-forward match the view, and (c) reveals the whole body.
/// `gloc`/`grot` are the game's POV location (UE units) and rotation (pitch,yaw,roll degrees).
pub fn control_pov(gloc: [f64; 3], grot: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let (rp, ry) = (grot[0].to_radians(), grot[1].to_radians());
    let f = [rp.cos() * ry.cos(), rp.cos() * ry.sin(), rp.sin()];
    let cam = [
        gloc[0] - f[0] * TP_BACK_UU,
        gloc[1] - f[1] * TP_BACK_UU,
        gloc[2] - f[2] * TP_BACK_UU + TP_UP_UU,
    ];
    (cam, grot) // look the same direction the unit aims, so the crosshair stays on target
}

fn boa_class() -> *mut u8 {
    let c = BOA.load(Ordering::Relaxed);
    if c != 0 {
        return c as *mut u8;
    }
    let p = find_class("BlamObjectActor");
    BOA.store(p as usize, Ordering::Relaxed);
    p
}

fn objc_class() -> *mut u8 {
    let c = OBJC.load(Ordering::Relaxed);
    if c != 0 {
        return c as *mut u8;
    }
    let p = find_class("BlamObjectComponent");
    OBJC.store(p as usize, Ordering::Relaxed);
    p
}

/// K2_GetActorLocation / K2_GetActorRotation live on AActor, so the same UFunction serves
/// every biped class - resolve once from the first target and cache.
fn loc_fn(actor: *mut u8) -> *mut u8 {
    let c = F_GET_LOC.load(Ordering::Relaxed);
    if c != 0 {
        return c as *mut u8;
    }
    let f = unsafe { find_function(class_of(actor), "K2_GetActorLocation") };
    F_GET_LOC.store(f as usize, Ordering::Relaxed);
    f
}

fn rot_fn(actor: *mut u8) -> *mut u8 {
    let c = F_GET_ROT.load(Ordering::Relaxed);
    if c != 0 {
        return c as *mut u8;
    }
    let f = unsafe { find_function(class_of(actor), "K2_GetActorRotation") };
    F_GET_ROT.store(f as usize, Ordering::Relaxed);
    f
}

/// FVector (3 x f64) at parms offset 0 for a no-arg getter like K2_GetActorLocation.
unsafe fn actor_vec3(actor: *mut u8, f: *mut u8) -> Option<[f64; 3]> {
    if actor.is_null() || f.is_null() {
        return None;
    }
    let mut buf = [0f64; 3];
    process_event(actor, f, buf.as_mut_ptr() as *mut c_void);
    Some(buf)
}

unsafe fn actor_location(actor: *mut u8) -> Option<[f64; 3]> {
    actor_vec3(actor, loc_fn(actor))
}

/// The target's eye POV (location with eye offset, facing rotation). Called every frame from
/// the camera detour; SEH-guarded and revalidated so a destroyed target degrades to `None`
/// (camera released) rather than faulting the whole override.
pub fn target_pov() -> Option<([f64; 3], [f64; 3])> {
    if !FOLLOW.load(Ordering::Relaxed) {
        return None;
    }
    // Anchor on the enemy biped actor (always valid in the puppet model - we never swap or write
    // +0x1BC, so it is never repurposed). compose_pov orbits from the mouse look, so rotation is
    // ignored here.
    let ptr = TARGET.load(Ordering::Relaxed) as *mut u8;
    if ptr.is_null() || object_at(TARGET_IDX.load(Ordering::Relaxed)) != ptr {
        return None;
    }
    let ctl = control_active();
    // Full-native camera: yaw+pitch from the SMOOTH aim input accumulators (element+0x94 yaw,
    // element+0x98 pitch) - mouse-integrated once per frame, upstream of the biped facing motor.
    // The yaw is NEGATED into the UE frame (verified from the log: sim facing -157 renders as +157,
    // so UE_yaw = -sim_yaw). We used to read the biped actor's UE rotation (K2_GetActorRotation),
    // which is the right frame but LAGS/oscillates as the body chases the aim at the turn rate ->
    // jittery camera. The negated smooth aim is both correctly framed (WASD/strafe match) AND smooth.
    // Spectate falls back to our own look state.
    let aim = if ctl { crate::simtime::player_look_angles() } else { None };
    let mut out = None;
    crate::seh::guard(|| unsafe {
        let Some(loc) = actor_vec3(ptr, loc_fn(ptr)) else {
            return;
        };
        let (yaw_d, pitch_d) = match aim {
            Some((yaw_r, pitch_r)) => (-(yaw_r.to_degrees() as f64), pitch_r.to_degrees() as f64),
            None => {
                let (y, p) = crate::look::yaw_pitch_deg();
                (y as f64, p as f64)
            }
        };
        out = Some(compose_pov(loc, yaw_d, pitch_d));
    });
    out
}

/// Build the camera POV from the target's origin location + a look direction (yaw/pitch in UE
/// degrees). Third-person: boom behind+above the eye, looking along the direction so the crosshair
/// tracks aim. forward = (cos p cos y, cos p sin y, sin p).
fn compose_pov(loc: [f64; 3], yaw_d: f64, pitch_d: f64) -> ([f64; 3], [f64; 3]) {
    let (ry, rp) = (yaw_d.to_radians(), pitch_d.to_radians());
    let eye = [loc[0], loc[1], loc[2] + EYE_UP];
    let (fx, fy, fz) = (rp.cos() * ry.cos(), rp.cos() * ry.sin(), rp.sin());
    if VIEW.load(Ordering::Relaxed) == VIEW_THIRD {
        let cam = [eye[0] - fx * TP_BACK, eye[1] - fy * TP_BACK, eye[2] - fz * TP_BACK + TP_UP];
        (cam, [pitch_d + TP_PITCH, yaw_d, 0.0])
    } else {
        let cam = [eye[0] + fx * FP_FORWARD, eye[1] + fy * FP_FORWARD, eye[2] + fz * FP_FORWARD];
        (cam, [pitch_d, yaw_d, 0.0])
    }
}

/// Set the view mode: "first" / "third" / "toggle".
pub fn set_view(which: &str) {
    let m = match which {
        "first" | "fp" | "1" => VIEW_FIRST,
        "third" | "tp" | "3" => VIEW_THIRD,
        _ => {
            if VIEW.load(Ordering::Relaxed) == VIEW_THIRD {
                VIEW_FIRST
            } else {
                VIEW_THIRD
            }
        }
    };
    VIEW.store(m, Ordering::Relaxed);
    crate::rep!("[possess] view: {}", if m == VIEW_THIRD { "third-person" } else { "first-person" });
}

/// Pick the best target biped (aimed via free-cam, else nearest to the player). Stores it and
/// returns the actor pointer, or null if none found. Game thread only.
fn pick_target(pc: *mut u8) -> *mut u8 {
    let boa = boa_class();
    if boa.is_null() {
        crate::rep!("[possess] BlamObjectActor class not found (load into a mission first)");
        return core::ptr::null_mut();
    }
    let objc = objc_class();

    // Ray origin/direction: the free-cam aim if active, else the player pawn's position.
    let st = crate::state::cam();
    let (mut ox, mut oy, mut oz, has_dir, dx, dy, dz) = if st.freecam {
        let ry = st.yaw.to_radians();
        let rp = st.pitch.to_radians();
        (st.x, st.y, st.z, true, rp.cos() * ry.cos(), rp.cos() * ry.sin(), rp.sin())
    } else {
        (0.0, 0.0, 0.0, false, 0.0, 0.0, 0.0)
    };
    drop(st);
    if !has_dir {
        let loc = unsafe { get_pawn(pc) };
        match unsafe { actor_location(loc) } {
            Some(l) => {
                ox = l[0];
                oy = l[1];
                oz = l[2];
            }
            None => {
                crate::rep!("[possess] press INSERT for free-cam, aim at an AI, then possess");
                return core::ptr::null_mut();
            }
        }
    }

    let mut best: *mut u8 = core::ptr::null_mut();
    let mut best_idx = -1i32;
    let mut best_score = f64::MAX;
    crate::seh::guard(|| unsafe {
        let n = num_elements();
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
            if !obj_name(class_of(o)).contains("BipedActor") {
                continue;
            }
            let oc = get_component(o, objc);
            if !oc.is_null() && call_ret_bool(oc, "IsDead") {
                continue;
            }
            let Some(l) = actor_location(o) else { continue };
            let (vx, vy, vz) = (l[0] - ox, l[1] - oy, l[2] - oz);
            let dist = (vx * vx + vy * vy + vz * vz).sqrt();
            if dist < 1.0 {
                continue; // that's us / co-located
            }
            let score = if has_dir {
                let along = (vx * dx + vy * dy + vz * dz) / dist; // cos(angle to aim)
                if along < 0.5 {
                    continue; // not in front of the camera
                }
                dist * (2.0 - along) // near + centered wins
            } else {
                dist
            };
            if score < best_score {
                best = o;
                best_idx = i;
                best_score = score;
            }
        }
    });

    if best.is_null() {
        crate::rep!("[possess] no living AI biped found in view");
        return core::ptr::null_mut();
    }
    TARGET.store(best as usize, Ordering::Relaxed);
    TARGET_IDX.store(best_idx, Ordering::Relaxed);
    best
}

/// Camera-only possession: lock the view to the target's eyes (safe / proven).
pub fn select_and_possess(pc: *mut u8) {
    let best = pick_target(pc);
    if best.is_null() {
        return;
    }
    // Seed the shared look from the camera's current facing so the orbit starts smooth.
    {
        let s = crate::state::cam();
        crate::look::seed(s.yaw.to_radians() as f32, s.pitch.to_radians() as f32);
    }
    crate::state::cam().freecam = false; // let the follow override own the camera
    VIEW.store(VIEW_THIRD, Ordering::Relaxed);
    FOLLOW.store(true, Ordering::Relaxed);
    unsafe {
        crate::rep!(
            "[possess] spectating {} ({}). Ctrl+H view, Ctrl+G release.",
            obj_name(class_of(best)),
            if VIEW.load(Ordering::Relaxed) == VIEW_THIRD { "3rd" } else { "1st" }
        );
    }
}

/// Tier 1: read a reflected datum-index UPROPERTY off the actor or its component, if one exists.
/// index (low 16) -> object-table record -> salt@rec+0 -> full handle. None if no such field.
fn handle_via_reflection(actor: *mut u8) -> Option<u32> {
    let objc = objc_class();
    let comp = if objc.is_null() { core::ptr::null_mut() } else { unsafe { get_component(actor, objc) } };
    const NAMES: &[&str] = &["ObjectDatumIndex", "BlamObjectIndex", "DatumIndex", "ObjectIndex", "ObjectId"];
    for obj in [actor, comp] {
        if obj.is_null() {
            continue;
        }
        let cls = unsafe { class_of(obj) };
        for nm in NAMES {
            let Some(off) = property_offset(cls, nm) else { continue };
            if !(0..0x2000).contains(&off) {
                continue;
            }
            let mut raw = 0xffff_ffffu32;
            let _ = crate::seh::guard(|| unsafe { raw = *((obj as usize + off as usize) as *const u32) });
            if raw == 0xffff_ffff {
                continue;
            }
            if let Some(full) = crate::simtime::normalize_to_full_handle(raw) {
                crate::rep!("[possess] reflected {nm} = 0x{raw:08x} -> handle 0x{full:08x}");
                return Some(full);
            }
        }
    }
    None
}

/// True if the sim body's vitality matches the UE component mirror (transform-free identity check).
fn fingerprint_agrees(actor: *mut u8, handle: u32) -> bool {
    let body = crate::simtime::object_body(handle);
    if !crate::simunit::valid_unit(body) {
        return false;
    }
    let (sbv, ssv) = crate::simtime::body_vitality(body);
    let objc = objc_class();
    if objc.is_null() {
        return true; // can't fingerprint; position already matched
    }
    let comp = unsafe { get_component(actor, objc) };
    if comp.is_null() {
        return true;
    }
    let mut cbv = f32::NAN;
    let mut csv = f32::NAN;
    let _ = crate::seh::guard(|| unsafe {
        cbv = *((comp as usize + COMP_BODY_VIT) as *const f32);
        csv = *((comp as usize + COMP_SHIELD_VIT) as *const f32);
    });
    let close = |a: f32, b: f32| a.is_finite() && b.is_finite() && (a - b).abs() < 5e-3;
    close(sbv, cbv) && (csv.is_nan() || close(ssv, csv))
}

/// Tier 2: displacement match from the player (the unknown coordinate offset cancels; only the
/// known 304.8 scale and a per-axis sign are needed - the sign is auto-resolved by fingerprint).
fn handle_via_position(pc: *mut u8, actor: *mut u8) -> Option<u32> {
    let sim_p = crate::simtime::player_sim_pos()?;
    let ue_p = unsafe { actor_location(get_pawn(pc)) }?;
    let ue_t = unsafe { actor_location(actor) }?;
    let d = [
        (ue_t[0] - ue_p[0]) as f32,
        (ue_t[1] - ue_p[1]) as f32,
        (ue_t[2] - ue_p[2]) as f32,
    ];
    const SIGNS: [[f32; 3]; 4] = [[1., -1., 1.], [1., 1., 1.], [-1., -1., 1.], [-1., 1., 1.]];
    let s = crate::simtime::WU_TO_UU;
    let mut best: Option<(u32, f32, usize)> = None; // handle, dist, sign index
    let mut units = 0u32;
    for (k, sg) in SIGNS.iter().enumerate() {
        let sim_t = [
            sim_p[0] + d[0] * sg[0] / s,
            sim_p[1] + d[1] * sg[1] / s,
            sim_p[2] + d[2] * sg[2] / s,
        ];
        if let Some((h, dist, c)) = crate::simtime::unit_handle_near_pos(sim_t) {
            units = c;
            if k == 0 {
                crate::rep!(
                    "[possess] sim_t=({:.2},{:.2},{:.2}) nearest 0x{h:08x} @ {dist:.2} wu (units={c})",
                    sim_t[0], sim_t[1], sim_t[2]
                );
            }
            if best.map_or(true, |(_, bd, _)| dist < bd) {
                best = Some((h, dist, k));
            }
        }
    }
    let Some((h, dist, k)) = best else {
        crate::rep!("[possess] object table has no units");
        return None;
    };
    // Log the fingerprint for information, but do NOT gate on it (a slightly-off validator
    // must not veto a correct sub-wu position match).
    let body = crate::simtime::object_body(h);
    let (sbv, ssv) = crate::simtime::body_vitality(body);
    let fp = fingerprint_agrees(actor, h);
    crate::rep!("[possess] best 0x{h:08x} @ {dist:.2} wu sign#{k} vit(body={sbv:.2},shield={ssv:.2}) fp={fp}");
    // Accept a confident position match, OR a farther one whose vitality fingerprint agrees - the
    // latter catches MOVING targets, whose interpolated UE position lags their authoritative sim
    // position by several wu (a running biped silently failed the old hard 1.0-wu gate -> spectate).
    if dist < 1.0 || (dist < 6.0 && fp) {
        Some(h)
    } else {
        crate::rep!("[possess] nearest unit {dist:.2} wu away (fp={fp}, units={units}) - NOT matched, spectate-only");
        None
    }
}

/// GATE 1: UE biped actor -> salted Blam unit handle. Tier 1 (reflection) then Tier 2 (position),
/// both validated by the vitality fingerprint. None if unresolved.
fn target_unit_handle(pc: *mut u8, actor: *mut u8) -> Option<u32> {
    if let Some(h) = handle_via_reflection(actor) {
        if fingerprint_agrees(actor, h) {
            return Some(h);
        }
        crate::rep!("[possess] reflected handle failed fingerprint - trying position");
    }
    handle_via_position(pc, actor)
}

/// Run any pending possession AI/slot work on the game-thread drain (called from pc_detour).
/// Targeted braindead (not a global freeze) so the possessed unit's player-control update -
/// which applies aim and pulls the trigger - stays alive while its own AI stops acting.
pub fn service(pc: *mut u8) {
    if AI_PENDING.swap(0, Ordering::Relaxed) == 2 {
        // Release: restore both native bindings to the player's real unit, un-hide Chief, revive
        // the enemy's AI. Runs on the game-thread drain so hs/pawn calls are safe.
        let h = CUR_HANDLE.swap(0xffff_ffff, Ordering::Relaxed);
        let old_p = SAVED_PLAYER_HANDLE.swap(0xffff_ffff, Ordering::Relaxed);
        let old_c = SAVED_CONTROL_HANDLE.swap(0xffff_ffff, Ordering::Relaxed);
        if old_p != 0xffff_ffff {
            crate::simtime::set_player_unit_handle(old_p);
        }
        if old_c != 0xffff_ffff {
            crate::simtime::set_control_unit_handle(old_c);
        }
        if h != 0xffff_ffff {
            crate::simtime::set_unit_owner(h, -1); // clear +0x1BC (undo full-native owner)
            // Re-attach the AI datum BEFORE un-braindead (set_ai_state resolves the actor via +0x1AC).
            crate::simtime::set_unit_ai_datum(h, SAVED_AI_DATUM.swap(0xffff_ffff, Ordering::Relaxed));
            crate::simtime::set_ai_state(h, BRAINDEAD_SAVED.load(Ordering::Relaxed));
        }
        // FACTION restore (SWARM6 primary): put the observer team byte (player+0xAC) back to the
        // saved UNSC value; the team-sync then copies it back into unit+0x1BA naturally.
        let saved_team = SAVED_PLAYER_TEAM.swap(i32::MIN, Ordering::Relaxed);
        if saved_team != i32::MIN {
            crate::simtime::restore_player_team_byte(saved_team as i8);
            crate::rep!("[possess] observer team restored -> {}", saved_team as i8);
        }
        crate::simtime::restore_turn_clamp(); // no-op (we no longer crank it); harmless
        crate::pawn::set_perspective(pc, 1); // back to first-person
        // (No relevancy restore: we no longer override the relevancy cvars on possess.)
        crate::hooks::sim_seed::uninstall(); // remove the sim-thread seed hook
        crate::pawn::hide_pawn(pc, false); // show Chief again
        PAWN_PTR.store(0, Ordering::Relaxed); // clear cached pawn
        crate::pawn::unfreeze_input(pc);
        crate::rep!("[possess] released - player unit 0x{old_p:08x} restored, Chief shown, enemy AI resumed");
    }
}

/// Per-frame while controlling: re-assert both unit bindings (the sim's input processing can
/// rewrite the control element each tick) and keep grenades stocked. Game thread only.
pub fn control_tick() {
    if !CONTROL.load(Ordering::Relaxed) {
        return;
    }
    let h = CUR_HANDLE.load(Ordering::Relaxed);
    if h == 0xffff_ffff {
        return;
    }
    // If the possessed unit died / its slot was freed, release cleanly instead of writing into
    // stale (possibly reused) sim memory - that corruption was crashing the sim.
    if !crate::simtime::handle_live(h) {
        crate::rep!("[possess] possessed unit is gone - releasing");
        CONTROL.store(false, Ordering::Relaxed);
        AI_PENDING.store(2, Ordering::Relaxed);
        return;
    }
    // Re-assert BOTH native bindings every tick - the sim's input processing rewrites the control
    // element (and can re-bind the player unit) each tick, so a one-shot bind decays. This is the
    // native drive: with these two slots pointed at the enemy, the engine walks/aims/fires it.
    // Movement + aim are now fully NATIVE (native mouse -> control rotation -> both), so we no
    // longer inject velocity/aim - the mouse hijack is off (input::poll_possess_look skips control).
    crate::simtime::set_player_unit_handle(h);
    crate::simtime::assert_control_unit_handle(h);
    // Re-assert +0x1BC so the unit stays on the native player pipeline. With this set, the ENGINE
    // drives aim (mouse turns body), camera-relative WASD, and fire - we inject nothing (injecting
    // fought the native writers and produced erratic movement). This is full-native control.
    if let Some(pi) = crate::simtime::local_player_index() {
        crate::simtime::set_unit_owner(h, pi);
    }
    // Keep the AI detached (body+0x1AC = -1) so the AI facing-controller never re-enters the stack
    // and re-introduces the frame-to-frame facing flip.
    crate::simtime::set_unit_ai_datum(h, 0xffff_ffff);
    // Re-assert the FACTION observer team (player+0xAC = possessed sect). Nothing writes +0xAC per
    // frame (the team-sync only READS it), so this is normally a no-op, but a checkpoint/respawn
    // mid-possession can re-stamp UNSC - so re-assert like every other bind. POSSESSED_TEAM holds the
    // sect to assert; SAVED_PLAYER_TEAM holds the UNSC value to restore on release (do not confuse).
    let sect = POSSESSED_TEAM.load(Ordering::Relaxed);
    if SAVED_PLAYER_TEAM.load(Ordering::Relaxed) != i32::MIN {
        crate::simtime::restore_player_team_byte((sect & 0xff) as i8);
    }

    // Move the streaming ANCHOR (Chief's real sim biped) to follow the possessed unit so the engine
    // streams + animates the AI around our position itself, instead of leaving them dormant/"skating"
    // because the anchor is frozen back at the possession spot. This is the crash-safe automation of
    // "walk Chief over": a PLAIN body+0x44 store on a live resolved object (NOT the gs:[]-resolving
    // teleport call, which would crash from the game thread; NOT force-activation, which crashed by
    // promoting unstreamed units). Chief's biped re-derives its cluster on the sim thread. SWARM8e.
    // Keep Chief's biped (streaming observer 0) ON the possessed unit (lead = 0) - this gives the
    // full near-field PVS coverage. Leading it forward regressed (a single observer trades near for
    // forward, and a lone injected cluster bit doesn't PVS-expand to replace it). The forward/
    // disconnected coverage is instead supplied by the sim-thread hook seeding the clusters of the
    // units AROUND us (see update_seeds). +1.0 wu up so his capsule doesn't shove us.
    let anchor_h = SAVED_PLAYER_HANDLE.load(Ordering::Relaxed);
    if anchor_h != 0xffff_ffff {
        let moved = crate::simtime::anchor_biped_to(anchor_h, h, 0.0, 1.0);
        ANCHOR_MOVED.store(moved.unwrap_or(-1.0).to_bits(), Ordering::Relaxed);
    }
    // Publish the possessed unit's cluster for the sim-thread seed hook to inject (game-thread read;
    // the relay only consumes the precomputed atomic).
    crate::hooks::sim_seed::update_seed(h);

    // Keep the enemy body owner-visible (cheap cached re-assert; tries to survive the +0x1BC retype).
    crate::pawn::reassert_body_visible();

    // Hold third-person perspective, but ONLY re-assert when the byte has actually DRIFTED from 2.
    // control_tick runs many times/frame; a blind per-tick SetCameraPerspective re-fired the
    // OnCameraPerspectiveChanged delegate + rep-updater several times/frame (the +0x28/+0x1BC churn
    // bounces the byte to 1), thrashing the camera manager -> sluggish rotate. Reading pawn+0x3C1
    // directly (plain byte - no ProcessEvent, no recursion) and calling the setter only on drift
    // fires it ~once/frame while keeping the weapon's 3P rep owner-visible. Cached pawn (never
    // get_pawn here - that ProcessEvent would re-enter pc_detour and infinitely recurse).
    {
        let pawn = PAWN_PTR.load(Ordering::Relaxed) as *mut u8;
        if !pawn.is_null() {
            // Keep Chief's escort pawn hidden (defensive re-assert; the pawn is dragged onto the unit).
            crate::pawn::reassert_pawn_hidden(pawn);
            let mut drifted = false;
            let _ = crate::seh::guard(|| unsafe {
                drifted = *((pawn as usize + crate::offsets::PAWN_PERSPECTIVE) as *const u8) != 2;
            });
            if drifted {
                crate::pawn::reassert_perspective(pawn, 2);
            }
        }
    }

    // Heavy diagnostic (throttled): bindings holding? native movement advancing pos? control
    // rotation tracking the mouse (drives movement/aim/camera together)?
    if DIAG_CTR.fetch_add(1, Ordering::Relaxed) % 60 == 0 {
        let ai = crate::simtime::ai_state(h);
        let body = crate::simtime::object_body(h);
        let bound_p = crate::simtime::player_unit_handle();
        let cr = crate::pawn::get_control_rotation(crate::pawn::pc());
        let (mut pos, mut rvel) = ([0f32; 3], [0f32; 3]);
        if body != 0 {
            let _ = crate::seh::guard(|| unsafe {
                let pp = (body + 0x44) as *const f32;
                pos = [*pp, *pp.add(1), *pp.add(2)];
                let vp = (body + 0x68) as *const f32;
                rvel = [*vp, *vp.add(1), *vp.add(2)];
            });
        }
        let (kw, ka, ks, kd, lmb) = (
            crate::input::held(0x57), crate::input::held(0x41), crate::input::held(0x53),
            crate::input::held(0x44), crate::input::held(0x01),
        );
        crate::rep!(
            "[nat] unit=0x{h:08x} boundP={bound_p:08x?} ai32c={ai:?} W{}A{}S{}D{} lmb={} cursor={} ctlrot(yaw,pitch)={cr:?}",
            kw as u8, ka as u8, ks as u8, kd as u8, lmb as u8, cursor_active() as u8
        );
        crate::rep!(
            "[nat] readvel=({:.2},{:.2},{:.2}) pos=({:.1},{:.1},{:.1}) weapon={:?}",
            rvel[0], rvel[1], rvel[2], pos[0], pos[1], pos[2], crate::simtime::weapon_diag(h)
        );
        // Anim-state validation (SWARM7_self_legs): (body+0xDC flags, body+0xE4 state, body+0xCC inst).
        // After the fix, +0xDC should read with bit 0x100 CLEAR and +0xE4 should be a real state
        // index (not 0x8000). If +0xCC ever reads 0xFFFFFFFF persistently, the instance was torn down.
        if let Some((dc, e4, cc)) = crate::simtime::anim_diag(h) {
            crate::rep!(
                "[nat-anim] +0xDC=0x{dc:08x} (fp-bit={}) +0xE4=0x{e4:04x} +0xCC=0x{cc:08x}",
                (dc & 0x100 != 0) as u8
            );
        }
        // Streaming-anchor follow (SWARM8e): distance Chief's biped jumped to re-anchor on us this
        // tick. Steady near 0 = anchor is pinned to the possessed unit; None (-1) = a body unresolved.
        crate::rep!(
            "[anchor] chief->unit jump={:.2} wu",
            f32::from_bits(ANCHOR_MOVED.load(Ordering::Relaxed))
        );
        // [cl]/[seed] SWARM9 streaming diag: cluster (bsp,clu,flags) of the anchor (Chief biped) vs
        // the possessed unit; the ACTIVE-cluster-set size (popcount = streaming zone breadth, the
        // number the second seed should raise); and the 4 co-op observer slots (only slot 0 populated
        // in SP -> single seed, the topology the WS3 hook widens). Read-only.
        crate::rep!(
            "[cl] chief={:?} unit={:?}",
            crate::simtime::object_cluster(SAVED_PLAYER_HANDLE.load(Ordering::Relaxed)),
            crate::simtime::object_cluster(h)
        );
        if let Some((pop, a0, b0)) = crate::simtime::active_cluster_popcount() {
            crate::rep!(
                "[seed] activeC_popcount={pop} seedA0=0x{a0:08x} seedB0=0x{b0:08x} obs={:08x?}",
                crate::simtime::observer_handles()
            );
        }
        // Faction validation: observer team (+0xAC) vs the possessed unit's target team (+0x1BA).
        // same=true => the are_enemies equality early-out fires FRIENDLY both ways (radar friendly +
        // the sect stops targeting you). The team-sync should carry +0x1BA to match +0xAC.
        let obs = crate::simtime::player_team_byte();
        let tgt = crate::simtime::unit_team(h).map(|w| (w & 0xff) as i8);
        crate::rep!("[faction] obs(+0xAC)={obs:?} tgt(+0x1BA)={tgt:?} same={}", obs == tgt);
        // Sim-thread seed hook: installed? relay fire count (should climb ~2/frame while possessing);
        // enabled = inject on. Rising fires with no crash validates the detour.
        crate::rep!(
            "[seedhook] installed={} on={} fires={} seeds={}",
            crate::hooks::sim_seed::installed(),
            crate::hooks::sim_seed::enabled(),
            crate::hooks::sim_seed::fires(),
            crate::hooks::sim_seed::seed_count()
        );
        // Frame comparison to pin the WASD-vs-camera issue: aim yaw (element+0x94 = the camera's
        // source), facing yaw (body+0x50 = the movement frame), velocity yaw (readvel). If velyaw
        // tracks aimyaw as the mouse turns, movement IS camera-relative; if it lags/diverges, it's
        // not. deg, atan2(y,x).
        // Does the (mesh-hidden) Chief pawn actually track the possessed unit? If pawn follows the
        // enemy, the render relevance anchor moves with us; if it stays put, we must teleport it.
        let target_ptr = TARGET.load(Ordering::Relaxed) as *mut u8;
        let (mut ppos, mut epos) = (None, None);
        let _ = crate::seh::guard(|| unsafe {
            let pawn = PAWN_PTR.load(Ordering::Relaxed) as *mut u8; // cached; never get_pawn here
            if !pawn.is_null() {
                ppos = actor_location(pawn);
            }
            if !target_ptr.is_null() {
                epos = actor_location(target_ptr);
            }
        });
        let pdist = match (ppos, epos) {
            (Some(a), Some(b)) => {
                Some(((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt())
            }
            _ => None,
        };
        crate::rep!("[nat3] pawn<->enemy dist={pdist:.0?} pawn={ppos:.0?} enemy={epos:.0?}");
        let aimyaw = crate::simtime::player_look_angles().map(|(y, _)| y.to_degrees());
        let facyaw = (crate::simtime::body_facing_diag(h))
            .map(|(f50, _, _)| (f50[1]).atan2(f50[0]).to_degrees());
        let velmag = (rvel[0] * rvel[0] + rvel[1] * rvel[1]).sqrt();
        let velyaw = if velmag > 0.3 { Some(rvel[1].atan2(rvel[0]).to_degrees()) } else { None };
        crate::rep!(
            "[nat2] aimyaw={aimyaw:.0?} facyaw={facyaw:.0?} velyaw={velyaw:.0?} |vel|={velmag:.1}"
        );
    }
}

/// Manually enable/disable AI while possessing (A/B test whether the global freeze is what
/// blocks aim/fire on the possessed unit). Runs on the game-thread drain.
pub fn set_ai(pc: *mut u8, on: bool) {
    crate::pawn::run_console_line(pc, if on { "hs:ai_enable 1" } else { "hs:ai_enable 0" });
    crate::rep!("[possess] AI {} (manual)", if on { "enabled" } else { "disabled" });
}

/// Dump the possessed unit's weapon chain for fire diagnosis.
pub fn diagweapon(_pc: *mut u8) {
    let h = CUR_HANDLE.load(Ordering::Relaxed);
    if h == 0xffff_ffff {
        crate::rep!("[dw] not controlling a unit - run 'huragok control' first");
        return;
    }
    crate::simtime::diag_weapon(h);
}

/// Diagnostic: dump the aimed target's datum-ish reflected properties + values, and the
/// player/target positions in both spaces. Informs Tier-1 existence and the sim<->UE transform.
pub fn diagunit(pc: *mut u8) {
    let best = pick_target(pc);
    if best.is_null() {
        return;
    }
    let objc = objc_class();
    let comp = if objc.is_null() { core::ptr::null_mut() } else { unsafe { get_component(best, objc) } };
    for (label, obj) in [("actor", best), ("component", comp)] {
        if obj.is_null() {
            continue;
        }
        let cls = unsafe { class_of(obj) };
        for (name, off) in list_properties(cls) {
            let key = ["Datum", "Index", "Handle", "Id", "Object"].iter().any(|k| name.contains(k));
            if !key || !(0..0x2000).contains(&off) {
                continue;
            }
            let mut v = 0u32;
            let _ = crate::seh::guard(|| unsafe { v = *((obj as usize + off as usize) as *const u32) });
            crate::rep!("[diag] {label}.{name} @ +0x{off:x} = 0x{v:08x}");
        }
    }
    let sp = crate::simtime::player_sim_pos();
    let up = unsafe { actor_location(get_pawn(pc)) };
    let ut = unsafe { actor_location(best) };
    crate::rep!("[diag] player sim_pos={sp:?}");
    crate::rep!("[diag] player ue_pos={up:?}");
    crate::rep!("[diag] target ue_pos={ut:?}");
}

/// Tier-3 possession: camera follow PLUS swap the player's sim unit to the target so your
/// input (WASD / aim / fire) drives it. Experimental - falls back to spectate if the handle
/// can't be read. Game thread only.
pub fn control(pc: *mut u8) {
    // Drop ImGui input/cursor if it's active, so possession starts cursor-hidden with native mouse
    // look live (otherwise the software cursor stays visible = the "hijacked cursor" complaint).
    if cursor_active() {
        crate::cmd::push(crate::cmd::Cmd::ImguiInput);
    }
    let best = pick_target(pc);
    if best.is_null() {
        return;
    }
    let cname = unsafe { obj_name(class_of(best)) };
    let Some(h) = target_unit_handle(pc, best) else {
        crate::rep!("[possess] *** {cname}: unit NOT resolved -> SPECTATE-ONLY (no bind, no HUD, no control). Aim at a more stationary enemy and Ctrl+G again. ***");
        crate::state::cam().freecam = false;
        VIEW.store(VIEW_THIRD, Ordering::Relaxed);
        FOLLOW.store(true, Ordering::Relaxed);
        return;
    };
    // Guard: never possess the player's OWN unit (the relaxed resolver can match Chief and flip us
    // into third-person on ourselves). Spectate instead so the user re-aims at an enemy.
    if crate::simtime::player_unit_handle() == Some(h) {
        crate::rep!("[possess] *** resolved to your OWN unit (0x{h:08x}) - not possessing. Aim at an enemy and Ctrl+G. ***");
        crate::state::cam().freecam = false;
        VIEW.store(VIEW_THIRD, Ordering::Relaxed);
        FOLLOW.store(true, Ordering::Relaxed);
        return;
    }
    // NATIVE-CONTROL model (per docs/research/ENEMY_MODEL_RENDER_FINDINGS.md). Raw body-field
    // injection can't move a biped - its locomotion controller zeroes our velocity every tick
    // (pos stays pinned; proven in the [pup] log). Instead we route the game's OWN player control
    // into the enemy unit via the two native bindings, so the engine walks/aims/fires it with full
    // animation and binds the HUD to its weapon/ammo:
    //   1. bind slot  players_base+idx*0x4B0+0x28  = movement/player-unit  (set_player_unit_handle)
    //   2. control element  +0x80                  = aim/fire target       (set_control_unit_handle)
    // We deliberately NEVER write the controlling-player datum unit+0x1BC: that is the ONLY thing
    // that retypes the enemy WorldRepresentation -> FirstPersonRepresentation (hides the model /
    // blinking). Left at -1, the enemy body stays fully visible in third person.
    crate::state::cam().freecam = false;
    VIEW.store(VIEW_THIRD, Ordering::Relaxed);
    CUR_HANDLE.store(h, Ordering::Relaxed);
    // Seed the mouse-orbit look from the unit's current facing so the camera doesn't snap.
    if let Some(f) = crate::simtime::body_forward(h) {
        crate::look::seed(f[1].atan2(f[0]), 0.0);
    }
    // Braindead the enemy's own AI (raw ai_actor+0x32C=0 poke) FIRST, while body+0x1AC still
    // resolves the AI actor.
    let saved = crate::simtime::set_ai_state(h, 0);
    BRAINDEAD_SAVED.store(saved.unwrap_or(1), Ordering::Relaxed);
    // THEN DETACH the AI actor: clear the unit's AI datum body+0x1AC (-1 = the spawn "no AI"
    // sentinel a real player unit has). THIS is the jitter fix (SWARM2_jitter_cause/braindead):
    // braindead alone leaves the AI facing-controller in the per-unit facing-function stack, and
    // the per-frame selector alternates it with the +0x1BC player-aim controller -> desired facing
    // flips frame to frame -> body+0x50 and the camera whip. Detaching makes the unit structurally
    // a native player unit so the player-aim path owns facing every frame. Saved for restore.
    // (No turn-clamp crank: native player aim doesn't route through the AI motor, so widening its
    // clamp only amplified the whip.)
    let old_ai = crate::simtime::set_unit_ai_datum(h, 0xffff_ffff).unwrap_or(0xffff_ffff);
    SAVED_AI_DATUM.store(old_ai, Ordering::Relaxed);
    // FACTION (kick-free): the radar classifier compares are_enemies(observer=player+0xAC,
    // target=unit+0x1BA). Forcing the target (unit team) did nothing because the OBSERVER stays the
    // player's UNSC team - and setting player+0xAC=Covenant ends the mission (verified kick). So do
    // NOT touch any team: ally the player and Covenant teams in the AI allegiance matrix via the
    // HaloScript verb - the radar's team-mode fall-through reads it and paints the sect friendly.
    // Reverted on release with ai_allegiance_remove. (No code patch, no team write, no kick.)
    // FACTION (SWARM6 primary; kick test confirmed SAFE): make the LOCAL PLAYER the observer of the
    // possessed unit's own sect. The radar/nav friend-foe classifiers key observer team on
    // player_struct+0xAC and target team on unit+0x1BA - writing +0xAC = the sect flips BOTH
    // directions at once (the sect paints friendly AND its AI stop reading us as enemy). It's
    // per-player, NOT team-vs-team, so marines stay at war -> no map-freeze. Leave the team-sync
    // UNPATCHED: its single writer copies +0xAC -> unit+0x1BA for free, so the possessed body's own
    // team also becomes the sect with no flicker race. (The prior lever forced unit+0x1BA directly +
    // patched the sync; it was shaky and never changed the observer, so the radar stayed red. Dropped.)
    if let Some(team) = crate::simtime::unit_team(h) {
        POSSESSED_TEAM.store(team as u32, Ordering::Relaxed); // ORIGINAL sect team (name lookup / re-assert)
        let sect = (team & 0xff) as i8;
        if let Some(old) = crate::simtime::set_player_team_byte(sect) {
            SAVED_PLAYER_TEAM.store(old as i32, Ordering::Relaxed); // UNSC observer team, to restore
            crate::rep!("[nat] faction: observer team {old}->{sect} (possessed sect friendly both ways)");
        }
    }
    // Now re-widen the facing-motor clamp so body+0x50 SNAPS to our aim. Movement rides body+0x50,
    // which otherwise turns at only ~120°/s, so during a mouse sweep the movement vector LAGS the
    // camera (log [nat2]: facyaw/velyaw trail aimyaw by 30-45deg). Snapping the facing makes WASD
    // track the camera like the main player. This is safe NOW because the jitter it amplified was
    // the AI-vs-player controller race, which the AI-detach above eliminated. Restored on release.
    crate::simtime::set_turn_clamp(20.0);
    // Bind both native slots to the enemy unit; save the prior handles to restore on release.
    let old_p = crate::simtime::set_player_unit_handle(h).unwrap_or(0xffff_ffff);
    let old_c = crate::simtime::set_control_unit_handle(h).unwrap_or(0xffff_ffff);
    SAVED_PLAYER_HANDLE.store(old_p, Ordering::Relaxed);
    SAVED_CONTROL_HANDLE.store(old_c, Ordering::Relaxed);
    // FULL-NATIVE control: set the controlling-player datum (+0x1BC) so the unit joins the player
    // aim-projection pipeline - native mouse turns the body, WASD is camera-relative, fire works,
    // ALL handled by the engine. Per SWARM_face_aim.md this is the ONLY way to get truly natural
    // control (bind-swap alone leaves facing/movement fighting us). It normally hides the body by
    // retyping the WorldRep; show_actor_body + the per-tick reassert try to keep it visible. If the
    // body cannot survive +0x1BC we'll see body_meshes fall to 0 and revert to the model-safe mode.
    if let Some(pi) = crate::simtime::local_player_index() {
        crate::simtime::set_unit_owner(h, pi);
        crate::rep!("[nat] +0x1BC owner=player idx {pi} (full-native control)");
    }
    // Cache the pawn pointer for the per-tick perspective hold + diagnostics, so control_tick never
    // calls get_pawn (a ProcessEvent that would re-enter pc_detour -> infinite recursion).
    PAWN_PTR.store(unsafe { crate::pawn::get_pawn(pc) } as usize, Ordering::Relaxed);
    // INTERIM: whole-actor-hide the Chief pawn again. Mesh-only hiding kept the pawn live as a
    // relevance anchor, but that left Chief's legs rendering (composited with the enemy) AND its
    // collision capsule co-located with the possessed unit jammed its movement (super-slow). Whole-
    // hide restores clean movement + no Chief; the nearby-unit flicker returns and is what the
    // SWARM4 agents are solving properly (relevance-anchor-without-a-live-pawn).
    crate::pawn::hide_pawn(pc, true);
    // NOTE: we NO LONGER override the Blam relevancy cvars. We used to set CullByDistance 0 /
    // DisableActorOnInactive 0 to stop nearby units popping when the hidden pawn didn't track us -
    // but that was before the anchor-follow (Chief's biped now rides the possessed unit, so the
    // relevance viewpoint tracks us and units stay relevant on their own). Worse, DisableActorOnInactive
    // 0 renders inactive actors WITHOUT simulating them, which was the CAUSE of the rigged/T-pose dead
    // marines. Leaving relevancy at its normal values fixes the corpses with no return of the blink
    // (confirmed live). SWARM10.
    // Switch the engine to third-person perspective (native SetCameraPerspective). Possession
    // otherwise leaves it in first-person while our camera is a TP boom, which arms UE's FP
    // near-field/relevance pass against the wrong origin -> nearby units' attachments (Jackal
    // shields, Hunter worms, whole Hunters) flicker. Perspective 2 aligns view-relevance with the
    // TP camera. Re-asserted each tick (it can bounce back to 1); restored to 1 on release.
    crate::pawn::set_perspective(pc, 2);
    // Reveal the enemy body: the bind sets bOwnerNoSee on its WorldRepresentation (that's why we
    // saw only the Jackal's shield), so force its body meshes owner-visible. control_tick re-asserts
    // this each tick because the mesh-sync layer re-hides it a frame or two after the bind.
    let shown = crate::pawn::show_actor_body(best, true);
    crate::rep!(
        "[nat] bind {cname} unit=0x{h:08x}  player:0x{old_p:08x}->0x{h:08x} ctl:0x{old_c:08x}->0x{h:08x}  braindead {saved:?}  body_meshes={shown}"
    );
    // Install the SWARM9 sim-thread seed hook so the streaming flood gets a SECOND seed (the
    // possessed unit's cluster) - the fix for edge units in PVS-disconnected clusters. The relay only
    // ORs a precomputed cluster bit (set per tick by update_seed) into the resident mask on the sim
    // thread. Removed on release. (Stage: with the anchor lead=0 the injected cluster == observer 0's,
    // so this is behaviourally a no-op that first proves the detour is crash-safe; then lead>0 makes
    // it a real union.)
    crate::hooks::sim_seed::install();
    CONTROL.store(true, Ordering::Relaxed);
    FOLLOW.store(true, Ordering::Relaxed);
    crate::rep!("[possess] NATIVE {cname}: WASD walks it, mouse turns/aims, click fires its weapon. Ctrl+G to release.");
}

/// Begin releasing control. The actual un-braindead + slot restore run on the next drain
/// (service, state 2) so the un-braindead targets the possessed unit while it is still current,
/// and no hs runs from the camera detour.
fn restore_control() {
    if CONTROL.swap(false, Ordering::Relaxed) {
        AI_PENDING.store(2, Ordering::Relaxed);
        crate::rep!("[possess] releasing control...");
    }
}

/// Stop following (and restore control) - hand the camera and input back to the game.
pub fn unpossess() {
    restore_control();
    if FOLLOW.swap(false, Ordering::Relaxed) {
        crate::rep!("[possess] released");
    }
}

/// Called by the camera detour when the target can no longer be read (e.g. it died).
pub fn on_target_lost() {
    restore_control();
    if FOLLOW.swap(false, Ordering::Relaxed) {
        crate::rep!("[possess] target lost - camera released");
    }
}

/// True while controlling a possessed unit.
pub fn controlling() -> bool {
    CONTROL.load(Ordering::Relaxed)
}


/// Ctrl+G handler: toggle full possession (control) of the aimed/nearest AI, or release.
pub fn toggle(pc: *mut u8) {
    if follow_active() {
        unpossess();
    } else {
        control(pc);
    }
}

// Throttled diagnostic counter for the puppet heartbeat log (see control_tick).
static DIAG_CTR: AtomicU32 = AtomicU32::new(0);
