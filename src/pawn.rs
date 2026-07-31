//! Executes queued commands on the game thread: cinematic mode, cheats, pawn FX,
//! third-person body, scale, time dilation, camera fades. Ported from the C++ drain.

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::cmd::Cmd;
use crate::mem::base;
use crate::offsets::*;
use crate::state::cam;
use crate::ue::process_event::{pe_call, process_event};
use crate::ue::reflect::{class_of, find_function, find_live_by_class};

// Shared flags read by the camera detour (time dilation, third-person hold, scale).
static PC: AtomicUsize = AtomicUsize::new(0);
static TD: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0f32 bits
static FULLBODY: AtomicBool = AtomicBool::new(false);
static SCALE: AtomicU64 = AtomicU64::new(0); // f64 bits; 0.0 = do not force
static TIME_APPLIED: AtomicBool = AtomicBool::new(false); // log the sim-clock reach once
static SKULL_OFFSET: AtomicUsize = AtomicUsize::new(0); // ActiveSkulls field offset + 1 (0 = unknown)

pub fn set_pc(p: *mut u8) {
    PC.store(p as usize, Ordering::Relaxed);
}
pub fn pc() -> *mut u8 {
    PC.load(Ordering::Relaxed) as *mut u8
}
pub fn time_dilation() -> f32 {
    f32::from_bits(TD.load(Ordering::Relaxed))
}
fn set_time_dilation(v: f32) {
    TD.store(v.to_bits(), Ordering::Relaxed);
}
pub fn scale() -> f64 {
    f64::from_bits(SCALE.load(Ordering::Relaxed))
}
pub fn fullbody() -> bool {
    FULLBODY.load(Ordering::Relaxed)
}

/// K2_GetPawn on a PlayerController.
pub unsafe fn get_pawn(pc: *mut u8) -> *mut u8 {
    if pc.is_null() {
        return core::ptr::null_mut();
    }
    let f = find_function(class_of(pc), "K2_GetPawn");
    if f.is_null() {
        return core::ptr::null_mut();
    }
    let mut buf = [0usize; 1];
    process_event(pc, f, buf.as_mut_ptr() as *mut c_void);
    buf[0] as *mut u8
}

/// Call a zero-arg UFunction on `obj` that returns an object pointer.
pub unsafe fn call_ret_ptr(obj: *mut u8, fname: &str) -> *mut u8 {
    if obj.is_null() {
        return core::ptr::null_mut();
    }
    let f = find_function(class_of(obj), fname);
    if f.is_null() {
        return core::ptr::null_mut();
    }
    let mut buf = [0u8; 32];
    process_event(obj, f, buf.as_mut_ptr() as *mut c_void);
    *(buf.as_ptr() as *const *mut u8)
}

/// Call a zero-arg UFunction on `obj` that returns an f32 (NaN if not found).
pub unsafe fn call_ret_f32(obj: *mut u8, fname: &str) -> f32 {
    if obj.is_null() {
        return f32::NAN;
    }
    let f = find_function(class_of(obj), fname);
    if f.is_null() {
        return f32::NAN;
    }
    let mut buf = [0u8; 32];
    process_event(obj, f, buf.as_mut_ptr() as *mut c_void);
    *(buf.as_ptr() as *const f32)
}

/// Call a zero-arg UFunction on `obj` that returns a bool.
pub unsafe fn call_ret_bool(obj: *mut u8, fname: &str) -> bool {
    if obj.is_null() {
        return false;
    }
    let f = find_function(class_of(obj), fname);
    if f.is_null() {
        return false;
    }
    let mut buf = [0u8; 32];
    process_event(obj, f, buf.as_mut_ptr() as *mut c_void);
    buf[0] != 0
}

/// Call a UFunction taking one actor pointer and returning a bool (e.g. team predicates).
pub unsafe fn call_actor_ret_bool(obj: *mut u8, fname: &str, actor: *mut u8) -> bool {
    if obj.is_null() {
        return false;
    }
    let f = find_function(class_of(obj), fname);
    if f.is_null() {
        return false;
    }
    let mut buf = [0u8; 32];
    *(buf.as_mut_ptr() as *mut *mut u8) = actor;
    process_event(obj, f, buf.as_mut_ptr() as *mut c_void);
    buf[8] != 0
}

/// AActor::GetComponentByClass(componentClass) -> component pointer.
pub unsafe fn get_component(actor: *mut u8, comp_class: *mut u8) -> *mut u8 {
    if actor.is_null() || comp_class.is_null() {
        return core::ptr::null_mut();
    }
    let f = find_function(class_of(actor), "GetComponentByClass");
    if f.is_null() {
        return core::ptr::null_mut();
    }
    let mut buf = [0u8; 32];
    *(buf.as_mut_ptr() as *mut *mut u8) = comp_class; // ComponentClass arg
    process_event(actor, f, buf.as_mut_ptr() as *mut c_void);
    *((buf.as_ptr() as usize + 8) as *const *mut u8) // return after the 8-byte arg
}

/// Run one command against the PlayerController `pc` (on the game thread).
pub fn execute(pc: *mut u8, c: Cmd) {
    unsafe {
        match c {
            Cmd::CineOn => {
                let mut p = [1u8, 1, 1, 1, 1];
                pe_call(pc, "SetCinematicMode", p.as_mut_ptr() as *mut c_void, 5);
            }
            Cmd::CineOff => {
                // affect-flags true so HUD/player/input come back
                let mut p = [0u8, 1, 1, 1, 1];
                pe_call(pc, "SetCinematicMode", p.as_mut_ptr() as *mut c_void, 5);
            }
            Cmd::Pause => {
                pe_call(pc, "Pause", core::ptr::null_mut(), 0);
            }
            Cmd::Freeze => {
                let mut pcp = pc;
                pe_call(pc, "DisableInput", &mut pcp as *mut _ as *mut c_void, 8);
                let mut t = 1u8;
                pe_call(pc, "SetIgnoreMoveInput", &mut t as *mut _ as *mut c_void, 1);
                pe_call(pc, "SetIgnoreLookInput", &mut t as *mut _ as *mut c_void, 1);
                crate::rep!("[freeze] input disabled");
            }
            Cmd::Unfreeze => {
                let mut pcp = pc;
                pe_call(pc, "EnableInput", &mut pcp as *mut _ as *mut c_void, 8);
                pe_call(pc, "ResetIgnoreInputFlags", core::ptr::null_mut(), 0);
                crate::rep!("[freeze] input enabled");
            }
            Cmd::Ghost | Cmd::Fly | Cmd::Walk | Cmd::God | Cmd::Ungod => {
                let pawn = get_pawn(pc);
                if pawn.is_null() {
                    crate::rep!("[cheat] no pawn");
                    return;
                }
                match c {
                    Cmd::Ghost => {
                        pe_call(pawn, "ClientCheatGhost", core::ptr::null_mut(), 0);
                        crate::rep!("[cheat] ghost/noclip");
                    }
                    Cmd::Fly => {
                        pe_call(pawn, "ClientCheatFly", core::ptr::null_mut(), 0);
                        crate::rep!("[cheat] fly");
                    }
                    Cmd::Walk => {
                        pe_call(pawn, "ClientCheatWalk", core::ptr::null_mut(), 0);
                        crate::rep!("[cheat] walk");
                    }
                    Cmd::God => {
                        let mut f = 0u8;
                        pe_call(pawn, "SetCanBeDamaged", &mut f as *mut _ as *mut c_void, 1);
                        crate::rep!("[cheat] god ON");
                    }
                    Cmd::Ungod => {
                        let mut f = 1u8;
                        pe_call(pawn, "SetCanBeDamaged", &mut f as *mut _ as *mut c_void, 1);
                        crate::rep!("[cheat] god OFF");
                    }
                    _ => {}
                }
            }
            Cmd::Fov(v) => {
                let mut vv = v;
                pe_call(pc, "FOV", &mut vv as *mut _ as *mut c_void, 4);
            }
            Cmd::PawnHide | Cmd::PawnShow | Cmd::PawnNoCol | Cmd::PawnCol => {
                let pawn = get_pawn(pc);
                if pawn.is_null() {
                    crate::rep!("[pawn] no pawn");
                    return;
                }
                match c {
                    Cmd::PawnHide => {
                        let mut t = 1u8;
                        pe_call(pawn, "SetActorHiddenInGame", &mut t as *mut _ as *mut c_void, 1);
                        crate::rep!("[pawn] hidden");
                    }
                    Cmd::PawnShow => {
                        let mut t = 0u8;
                        pe_call(pawn, "SetActorHiddenInGame", &mut t as *mut _ as *mut c_void, 1);
                        crate::rep!("[pawn] shown");
                    }
                    Cmd::PawnNoCol => {
                        let mut t = 0u8;
                        pe_call(pawn, "SetActorEnableCollision", &mut t as *mut _ as *mut c_void, 1);
                        crate::rep!("[pawn] collision off");
                    }
                    Cmd::PawnCol => {
                        let mut t = 1u8;
                        pe_call(pawn, "SetActorEnableCollision", &mut t as *mut _ as *mut c_void, 1);
                        crate::rep!("[pawn] collision on");
                    }
                    _ => {}
                }
            }
            Cmd::ScaleGiant => {
                SCALE.store(3.0f64.to_bits(), Ordering::Relaxed);
                crate::rep!("[scale] giant x3 (world-rep actor)");
            }
            Cmd::ScaleTiny => {
                SCALE.store(0.3f64.to_bits(), Ordering::Relaxed);
                crate::rep!("[scale] tiny x0.3 (world-rep actor)");
            }
            Cmd::ScaleNormal => {
                SCALE.store(1.0f64.to_bits(), Ordering::Relaxed);
                crate::rep!("[scale] normal x1");
            }
            Cmd::CamoOn
            | Cmd::CamoOff
            | Cmd::CamoToggle
            | Cmd::OvershieldOn
            | Cmd::OvershieldOff
            | Cmd::FreezePawn
            | Cmd::UnfreezePawn
            | Cmd::ShieldBreak
            | Cmd::NightVis
            | Cmd::RechargePP
            | Cmd::BloodHuman
            | Cmd::BloodCov
            | Cmd::BloodGrunt
            | Cmd::BloodBrute
            | Cmd::Teleport
            | Cmd::RadialBlur
            | Cmd::Breath => {
                let pawn = get_pawn(pc);
                if pawn.is_null() {
                    crate::rep!("[pawn] no pawn");
                    return;
                }
                pawn_fx(pawn, c);
            }
            Cmd::FullbodyOn => set_perspective(pc, 2),  // ThirdPerson
            Cmd::FullbodyOff => set_perspective(pc, 1), // FirstPerson
            Cmd::ImguiInput => run_console(pc, "ImGui.ToggleInput"),
            Cmd::Console(s) => run_console(pc, s),
            Cmd::Slomo(v) => {
                set_time_dilation(v);
                // Drive the real Blam sim clock: game_speed in game_time_globals (the sim
                // runs in its own module on its own clock). apply()
                // resolves the clock itself; the per-frame hold_time re-asserts it.
                let landed = crate::simtime::apply(v);
                if !TIME_APPLIED.swap(true, Ordering::Relaxed) {
                    match crate::simtime::read() {
                        Some((tr, tl, gs)) => crate::rep!(
                            "[time] sim clock {}: tick_rate={} tick_length={:.5} game_speed now {:.2} (scale {:.2})",
                            if landed { "reached" } else { "NOT reached" }, tr, tl, gs, v
                        ),
                        None => crate::rep!("[time] sim clock not resolved yet"),
                    }
                }
            }
            Cmd::SimFreeze => blam_pause(pc, true),
            Cmd::SimUnfreeze => blam_pause(pc, false),
            Cmd::DiagTime => diag_time(pc),
            Cmd::DiagSkulls => diag_skulls(pc),
            Cmd::DiagView => diag_view(pc),
            Cmd::DiagMission => crate::campaign::diag_mission(pc),
            Cmd::SkullBit(v, on) => skull_set_bit(pc, v, on),
            Cmd::SimSkull(bit, on) => {
                crate::simtime::set_sim_skull(bit, on);
            }
            Cmd::NightVision(on) => {
                // ON: let the game's skull blueprint spawn the NV post-process actor; then
                // enable its component. OFF: disable the spawned component (the real off -
                // the skull blueprint has no despawn path, which is why nothing else worked).
                if on {
                    run_console(pc, "hs:skull_enable skull_night_vision true");
                }
                set_nv_postprocess(pc, on);
            }
            Cmd::FadeOut => camera_fade(true),
            Cmd::FadeIn => camera_fade(false),
        }
    }
}

/// Pawn FX (materials, blood, scale target, teleport). SEH-guarded: many of these
/// deref lazily-created materials and fault if called cold.
fn pawn_fx(pawn: *mut u8, c: Cmd) {
    let ran = crate::seh::guard(|| unsafe {
        let mut buf = [0u8; 64];
        let b = buf.as_mut_ptr();
        match c {
            Cmd::CamoOn => {
                pe_call(pawn, "Camo_RecacheMeshes", core::ptr::null_mut(), 0);
                let mut idx = 3i32;
                pe_call(pawn, "Camo_UpdateIndex", &mut idx as *mut _ as *mut c_void, 4);
                pe_call(pawn, "ActorApplyMaskedMaterials", core::ptr::null_mut(), 0);
            }
            Cmd::CamoOff => {
                pe_call(pawn, "ActorApplyOpaqueMaterials", core::ptr::null_mut(), 0);
            }
            Cmd::CamoToggle => {
                pe_call(pawn, "Camo_Toggle", b as *mut c_void, 8);
            }
            Cmd::OvershieldOn => {
                *b = 1;
                pe_call(pawn, "Overshield_Toggle", b as *mut c_void, 1);
            }
            Cmd::OvershieldOff => {
                *b = 0;
                pe_call(pawn, "Overshield_Toggle", b as *mut c_void, 1);
            }
            Cmd::FreezePawn => {
                *b = 0;
                pe_call(pawn, "SetActorTickEnabled", b as *mut c_void, 1);
            }
            Cmd::UnfreezePawn => {
                *b = 1;
                pe_call(pawn, "SetActorTickEnabled", b as *mut c_void, 1);
            }
            Cmd::ShieldBreak => {
                pe_call(pawn, "ShieldBreakPP", core::ptr::null_mut(), 0);
            }
            Cmd::NightVis => {
                pe_call(pawn, "NightVisionReloadCheck", core::ptr::null_mut(), 0);
            }
            Cmd::RechargePP => {
                pe_call(pawn, "PlayRechargeScreenPP", core::ptr::null_mut(), 0);
            }
            Cmd::BloodHuman => {
                pe_call(pawn, "ScreenBloodHuman", b as *mut c_void, 16);
            }
            Cmd::BloodCov => {
                pe_call(pawn, "ScreenBloodCov", b as *mut c_void, 16);
            }
            Cmd::BloodGrunt => {
                pe_call(pawn, "ScreenBloodGrunt", b as *mut c_void, 16);
            }
            Cmd::BloodBrute => {
                pe_call(pawn, "ScreenBloodBrute", b as *mut c_void, 16);
            }
            Cmd::Teleport => {
                let s = cam();
                let v = b as *mut f64;
                *v = s.x;
                *v.add(1) = s.y;
                *v.add(2) = s.z;
                let r = b.add(0x18) as *mut f64;
                *r.add(1) = s.yaw;
                drop(s);
                pe_call(pawn, "K2_TeleportTo", b as *mut c_void, 49);
            }
            Cmd::RadialBlur => {
                pe_call(pawn, "VehicleRadialBlurPPEffect", b as *mut c_void, 8);
            }
            Cmd::Breath => {
                pe_call(pawn, "Biped_BreathEffectToggle", b as *mut c_void, 24);
            }
            _ => {}
        }
    });
    if ran {
        crate::rep!("[pawnfx] {:?} ok", c);
    } else {
        crate::rep!("[pawnfx] {:?} faulted - skipped (needs game state)", c);
    }
}

/// Switch the pawn's camera perspective via the native `ABlamPawn::SetCameraPerspective`
/// (1 = first-person, 2 = third-person). This is the real path (confirmed by disassembly):
/// it writes the authoritative perspective field, runs the inner driver that swaps the
/// first/third meshes and weapon actors, and broadcasts the perspective-changed multicast
/// delegate that makes the camera manager actually move the camera. A raw field write does
/// none of that, which is why every earlier attempt reverted.
///
/// Two rules from the disassembly: (1) do NOT pre-write pawn+0x3C1 - the function early-
/// outs if it already equals the requested value; (2) open the third-person show gate
/// (repmgr+0x13c) the inner driver reads, or the body/weapon can stay hidden.
fn set_perspective(pc: *mut u8, persp: u8) {
    let ran = crate::seh::guard(|| unsafe {
        let pawn = get_pawn(pc);
        if pawn.is_null() {
            crate::rep!("[view] no pawn");
            return;
        }
        if persp == 2 {
            let rep = *((pawn as usize + PAWN_REPMGR) as *const *mut u8);
            if !rep.is_null() {
                *((rep as usize + REPMGR_GATE_SHOW) as *mut u8) = 1;
            }
        }
        type SetPersp = unsafe extern "system" fn(*mut u8, u8, *mut i32);
        let f: SetPersp = core::mem::transmute(base() + SET_CAMERA_PERSPECTIVE);
        let mut ctx = [-1i32, -1i32];
        f(pawn, persp, ctx.as_mut_ptr());
        let cur = *((pawn as usize + PAWN_PERSPECTIVE) as *const u8);
        crate::rep!("[view] SetCameraPerspective({}) -> perspective now {}", persp, cur);
    });
    if !ran {
        crate::rep!("[view] set-perspective faulted");
    }
}

/// Run a console command line (from the console input reader), via ExecuteConsoleCommand.
pub fn run_console_line(pc: *mut u8, cmd: &str) {
    run_console(pc, cmd);
}

/// Third-person body, surgically. The pawn owns EIGHT skeletal meshes: first-person
/// arm meshes (`BPC_FP_*`), a first-person shadow proxy, translucent camo overlays,
/// and the real third-person body meshes (`BPC_PAWN_SkeletalMesh_C`, `BPC_SkeletalMesh_C`,
/// `Body`). The clean approach is to flip ONLY `bOwnerNoSee` and touch NOTHING else:
///   - non-FP body meshes -> bOwnerNoSee=false so your own camera sees the body;
///   - FP meshes          -> bOwnerNoSee=true  so the first-person arms/shadow do not
///     float alongside as the "duplicate".
/// We do NOT force SetVisibility/SetHiddenInGame anymore: that was revealing the shadow
/// and translucent proxy meshes as the wireframe cages / ghost overlay. Each mesh keeps
/// its game-managed visibility (translucent stays hidden unless camo is active, etc.).
pub fn try_third_person(pc: *mut u8, on: bool) {
    let ran = crate::seh::guard(|| unsafe {
        let pawn = get_pawn(pc);
        if pawn.is_null() {
            crate::rep!("[tp] no pawn");
            return;
        }
        let actor = call_ret_ptr(pawn, "GetBlamObjectActor");
        crate::rep!("[tp] pawn={:p} actor={:p} show={}", pawn, actor, on);

        // Helpers: bOwnerNoSee, and a combined visibility set for the mesh ITSELF only.
        // bPropagateToChildren is 0 on purpose: propagating un-hides child components
        // (the weapon and its collision proxy attached to a hand socket), which rendered
        // as wireframe cages. We only want the body mesh visible, not its attachments.
        let set_owner_no_see = |o: *mut u8, no_see: bool| {
            let mut b = no_see as u8;
            pe_call(o, "SetOwnerNoSee", &mut b as *mut u8 as *mut c_void, 1);
        };
        let set_visible = |o: *mut u8, visible: bool| {
            let mut vis = [visible as u8, 0u8];
            pe_call(o, "SetVisibility", vis.as_mut_ptr() as *mut c_void, 2);
            let mut hidden = [(!visible) as u8, 0u8];
            pe_call(o, "SetHiddenInGame", hidden.as_mut_ptr() as *mut c_void, 2);
        };

        let n = crate::ue::object::num_elements();
        let mut toggled = 0;
        for i in 0..n {
            let o = crate::ue::object::object_at(i);
            if o.is_null() {
                continue;
            }
            // Owned by the pawn/actor within a few Outer hops. Walking the chain (not just
            // direct ownership) catches sub-meshes like the head, whose Outer is the body
            // mesh (head -> body mesh -> pawn). The weapon's collision proxy is owned by a
            // separate weapon actor, so its chain never reaches the pawn - it stays
            // untouched, which is why we can show the head without the weapon wireframe.
            let mut owner = *((o as usize + crate::offsets::UO_OUTER) as *const *mut u8);
            let mut owned = false;
            for _ in 0..5 {
                if owner.is_null() {
                    break;
                }
                if owner == pawn || owner == actor {
                    owned = true;
                    break;
                }
                owner = *((owner as usize + crate::offsets::UO_OUTER) as *const *mut u8);
            }
            if !owned {
                continue;
            }
            let name = crate::ue::fname::obj_name(o);
            let cn = crate::ue::fname::obj_name(class_of(o));
            if !cn.contains("SkeletalMesh") && !name.contains("SkeletalMesh") {
                continue;
            }
            // Proxy meshes we must NOT show in third person: first-person arms (`FP`),
            // the shadow-only proxy (`Shadow`), and the camo/ghost overlay (`Translucent`).
            // Everything else (`BPC_PAWN_SkeletalMesh_C`, `BPC_SkeletalMesh_C`, `Body`) is
            // the real third-person body.
            let is_proxy =
                name.contains("FP") || name.contains("Shadow") || name.contains("Translucent");
            let is_fp = name.contains("FP");
            if on {
                if is_proxy {
                    set_visible(o, false); // hide arms / shadow / ghost overlay
                } else {
                    set_owner_no_see(o, false); // real body: let the owner camera see it
                    set_visible(o, true);
                }
            } else if is_fp {
                set_owner_no_see(o, false); // restore first-person arms
                set_visible(o, true);
            } else if !is_proxy {
                set_owner_no_see(o, true); // hide the third-person body from owner again
            }
            crate::rep!("[tp] {} proxy={} show={}", name, is_proxy, on);
            toggled += 1;
        }
        crate::rep!("[tp] {toggled} skeletal mesh(es) adjusted show={on}");
    });
    if !ran {
        crate::rep!("[tp] faulted");
    }
}

/// Activate or restore the third-person world-representation body. SEH-guarded.
pub fn show_full_body(pc: *mut u8, on: bool) {
    let pawn = unsafe { get_pawn(pc) };
    if pawn.is_null() {
        crate::rep!("[fullbody] no pawn");
        return;
    }
    let ok = crate::seh::guard(|| unsafe {
        type Updater = unsafe extern "system" fn(*mut u8, i32, i32);
        type GetRep = unsafe extern "system" fn(*mut u8, i32) -> *mut u8;
        let updater: Updater = core::mem::transmute(base() + REP_UPDATER);
        let get_rep: GetRep = core::mem::transmute(base() + GET_REP_BY_INDEX);
        let repmgr = *((pawn as usize + PAWN_REPMGR) as *const *mut u8);
        let idx_before = *((pawn as usize + PAWN_ACTIVE_REP) as *const i32);
        if on {
            let rep0 = get_rep(pawn, 0);
            let rep1 = get_rep(pawn, 1);
            crate::rep!(
                "[fullbody] STATE idx={} persp={} repmgr={:p} rep0={:p} rep1={:p}",
                idx_before,
                *((pawn as usize + PAWN_PERSPECTIVE) as *const u8),
                repmgr,
                rep0,
                rep1
            );
            if !repmgr.is_null() {
                *((repmgr as usize + REPMGR_ACTIVE_WORLD_REP) as *mut i32) = 1;
                *((repmgr as usize + REPMGR_GATE_SHOW) as *mut u8) = 1;
            }
            *((pawn as usize + PAWN_ACTIVE_REP) as *mut i32) = 1;
            *((pawn as usize + PAWN_PERSPECTIVE) as *mut u8) = 2;
            updater(pawn, 1, -1);
        } else {
            if !repmgr.is_null() {
                *((repmgr as usize + REPMGR_ACTIVE_WORLD_REP) as *mut i32) = -1;
            }
            *((pawn as usize + PAWN_ACTIVE_REP) as *mut i32) = 0;
            *((pawn as usize + PAWN_PERSPECTIVE) as *mut u8) = 1;
            updater(pawn, 0, -1);
        }
    });
    crate::rep!("[fullbody] {} {}", if on { "ON" } else { "OFF" }, if ok { "done" } else { "faulted" });
}

/// Re-assert the Blam sim time scale every frame. The sim rewrites `tick_length` each
/// tick, so a one-shot write does not stick; we re-apply the current scale (and, once the
/// user returns to 1.0, keep writing the normal value so it restores cleanly). No-op until
/// the Time slider has been touched at least once.
pub fn hold_time() {
    if TIME_APPLIED.load(Ordering::Relaxed) {
        // User is scaling time: hold their game_speed each frame.
        crate::simtime::apply(time_dilation());
    } else {
        // Not scaling: keep tick_length stock so a game-restored fast value (boot or
        // after a death/checkpoint reload) can never leave the sim running fast.
        crate::simtime::pin_tick_length();
    }
}

/// Re-assert third-person body and forced scale every frame (pawn tick resets them).
/// Called from the camera detour, which runs after the pawn tick.
pub fn hold_pawn_state() {
    if !FULLBODY.load(Ordering::Relaxed) && scale() == 0.0 {
        return;
    }
    let pawn = unsafe { get_pawn(pc()) };
    if pawn.is_null() {
        return;
    }
    crate::seh::guard(|| unsafe {
        let repmgr = *((pawn as usize + PAWN_REPMGR) as *const *mut u8);
        if FULLBODY.load(Ordering::Relaxed) {
            type Updater = unsafe extern "system" fn(*mut u8, i32, i32);
            let updater: Updater = core::mem::transmute(base() + REP_UPDATER);
            if !repmgr.is_null() {
                *((repmgr as usize + REPMGR_ACTIVE_WORLD_REP) as *mut i32) = 1;
                *((repmgr as usize + REPMGR_GATE_SHOW) as *mut u8) = 1;
            }
            *((pawn as usize + PAWN_ACTIVE_REP) as *mut i32) = 1;
            *((pawn as usize + PAWN_PERSPECTIVE) as *mut u8) = 2;
            updater(pawn, 1, -1);
        }
        let s = scale();
        if s != 0.0 {
            type GetRep = unsafe extern "system" fn(*mut u8, i32) -> *mut u8;
            let get_rep: GetRep = core::mem::transmute(base() + GET_REP_BY_INDEX);
            let wr = get_rep(pawn, 1);
            if !wr.is_null() {
                let f = find_function(class_of(wr), "SetActorScale3D");
                if !f.is_null() {
                    let mut sb = [0u8; 24];
                    let v = sb.as_mut_ptr() as *mut f64;
                    *v = s;
                    *v.add(1) = s;
                    *v.add(2) = s;
                    process_event(wr, f, sb.as_mut_ptr() as *mut c_void);
                }
            }
        }
    });
}

/// Probe the Blam skull system so we can wire ThirdPerson / NightVision / cheats: find the
/// live `BlamSkullsGameStateComponent`, list its functions (in case there is a setter),
/// read the current `ActiveSkulls` value, locate its field offset, and dump the
/// `EBlamGameSkulls` enum bit values. Read-only. SEH-guarded.
fn diag_skulls(_pc: *mut u8) {
    use crate::ue::fname::obj_name;

    let comp = find_live_by_class("BlamSkullsGameStateComponent");
    if comp.is_null() {
        crate::rep!("[skull] BlamSkullsGameStateComponent not live");
        return;
    }
    let cls = unsafe { class_of(comp) };
    crate::rep!("[skull] component @ {:p} class={}", comp, unsafe { obj_name(cls) });

    match crate::ue::reflect::property_offset(cls, "ActiveSkulls") {
        Some(off) if (0..0x4000).contains(&off) => {
            SKULL_OFFSET.store(off as usize + 1, Ordering::Relaxed);
            crate::seh::guard(|| unsafe {
                // ActiveSkulls is a 32-byte (256-bit) bitset; dump it as 4 u64 words.
                let base = comp as usize + off as usize;
                let w0 = *(base as *const u64);
                let w1 = *((base + 8) as *const u64);
                let w2 = *((base + 16) as *const u64);
                let w3 = *((base + 24) as *const u64);
                crate::rep!(
                    "[skull] ActiveSkulls @ +0x{:x} = {:016x} {:016x} {:016x} {:016x}",
                    off, w0, w1, w2, w3
                );
            });
        }
        other => crate::rep!("[skull] ActiveSkulls offset unresolved ({:?})", other),
    }
}

/// Toggle the night-vision post-process overlay via UE reflection (safe - no sim memory
/// pokes). Two systems exist: a placed `BP_NightVision_V2` actor with a UPostProcessComponent,
/// and the pawn's `NightVisionDMI` dynamic material blended onto the camera. We drive both:
/// the placed actor's component (BlendWeight/bEnabled) and the pawn DMI's `Opacity` scalar.
fn set_nv_postprocess(pc: *mut u8, on: bool) {
    let ran = crate::seh::guard(|| unsafe {
        use crate::ue::object::{num_elements, object_at};
        use crate::ue::reflect::{class_chain_has, property_offset};

        // --- System B: placed BP_NightVision_V2 actor's post-process component ---
        let actor = find_live_by_class("BP_NightVision");
        if !actor.is_null() {
            let n = num_elements();
            let mut comp: *mut u8 = core::ptr::null_mut();
            for i in 0..n {
                let o = object_at(i);
                if o.is_null() {
                    continue;
                }
                let outer = *((o as usize + UO_OUTER) as *const *mut u8);
                if outer == actor && class_chain_has(class_of(o), "PostProcessComponent") {
                    comp = o;
                    break;
                }
            }
            if !comp.is_null() {
                let cls = class_of(comp);
                if let Some(off) = property_offset(cls, "BlendWeight") {
                    if (0..0x4000).contains(&off) {
                        *((comp as usize + off as usize) as *mut f32) = if on { 1.0 } else { 0.0 };
                    }
                }
                if let Some(off) = property_offset(cls, "bEnabled") {
                    if (0..0x4000).contains(&off) {
                        *((comp as usize + off as usize) as *mut u8) = on as u8;
                    }
                }
                crate::rep!("[nv] PPV component {:p} -> {}", comp, if on { "on" } else { "off" });
            } else {
                crate::rep!("[nv] BP_NightVision actor has no PostProcessComponent");
            }
        } else if !on {
            crate::rep!("[nv] no BP_NightVision actor to disable (not spawned?)");
        }
    });
    if !ran {
        crate::rep!("[nv] post-process toggle faulted");
    }
    let _ = pc;
}

/// Probe the third-person / view-mode mechanism: enumerate UFunctions on the pawn and its
/// Blam actor whose names hint at camera perspective / view mode, and dump the
/// `EBlamCameraPerspective` enum values. The Blueprint (`GetPawnViewModeAndWeaponActors`,
/// `Switch on EBlamCameraPerspective`) proves this system exists; we want its setter.
fn diag_view(pc: *mut u8) {
    use crate::ue::fname::{name_by_id, obj_name};
    use crate::ue::object::{num_elements, object_at};
    use crate::ue::reflect::super_of;

    let pawn = unsafe { get_pawn(pc) };
    if pawn.is_null() {
        crate::rep!("[view] no pawn");
        return;
    }
    let actor = unsafe { call_ret_ptr(pawn, "GetBlamObjectActor") };
    crate::rep!("[view] pawn @ {:p} class={} actor @ {:p}", pawn, unsafe { obj_name(class_of(pawn)) }, actor);

    let needles = [
        "Perspective", "ViewMode", "View", "Camera", "Persp", "Vision", "Night",
        "Spectate", "ThirdPerson", "FirstPerson", "Weapon", "Skull", "Cheat", "Ammo",
    ];
    for (label, obj) in [("pawn", pawn), ("actor", actor)] {
        if obj.is_null() {
            continue;
        }
        crate::seh::guard(|| unsafe {
            let mut c = class_of(obj);
            for _ in 0..10 {
                if c.is_null() {
                    break;
                }
                let mut child = *((c as usize + US_CHILDREN) as *const *mut u8);
                let mut steps = 0;
                while !child.is_null() && steps < 8192 {
                    if obj_name(class_of(child)) == "Function" {
                        let nm = obj_name(child);
                        if needles.iter().any(|s| nm.contains(s)) {
                            let ps = *((child as usize + UFN_PARMSSIZE) as *const u16);
                            crate::rep!("[view] {} fn {}::{} ParmsSize={}", label, obj_name(c), nm, ps);
                        }
                    }
                    child = *((child as usize + UF_NEXT) as *const *mut u8);
                    steps += 1;
                }
                c = super_of(c);
            }
        });
    }

    crate::seh::guard(|| unsafe {
        let n = num_elements();
        for i in 0..n {
            let o = object_at(i);
            if o.is_null() {
                continue;
            }
            if obj_name(class_of(o)) == "Enum" && obj_name(o) == "EBlamCameraPerspective" {
                let data = *((o as usize + 0x40) as *const *const u8);
                let num = *((o as usize + 0x48) as *const i32);
                crate::rep!("[view] EBlamCameraPerspective Num={}", num);
                if !data.is_null() && num > 0 && num < 64 {
                    for j in 0..num as usize {
                        let e = data.add(j * 16);
                        let id = *(e as *const u32);
                        let val = *((e as usize + 8) as *const i64);
                        crate::rep!("[view] {} = {}", name_by_id(id), val);
                    }
                }
                break;
            }
        }
    });
}

/// Set or clear a skull's bit in `BlamSkullsGameStateComponent.ActiveSkulls` (the enum
/// value is the bit index, e.g. NightVision=40, ThirdPerson=55, Bandana=18). Requires the
/// offset resolved by [`diag_skulls`] first. Whether flipping the bit actually triggers
/// the skull depends on the game re-reading it - this is the test.
fn skull_set_bit(pc: *mut u8, value: u8, on: bool) {
    let ran = crate::seh::guard(|| unsafe {
        // 1) Keep the component's ActiveSkulls bitset consistent (if the offset resolved).
        let raw = SKULL_OFFSET.load(Ordering::Relaxed);
        if raw != 0 {
            let comp = find_live_by_class("BlamSkullsGameStateComponent");
            if !comp.is_null() {
                let p = ((comp as usize + (raw - 1)) + value as usize / 8) as *mut u8;
                let mask = 1u8 << (value % 8);
                *p = if on { *p | mask } else { *p & !mask };
            }
        }
        // 2) Invoke the pawn's skull-apply event with a 32-byte bitset holding just this
        //    skull. OnSkullsAdded/OnSkullsRemoved are what the game calls to actually apply
        //    a skull's effect (the field write alone is inert). ParmsSize 32 = the bitset.
        let pawn = get_pawn(pc);
        if pawn.is_null() {
            crate::rep!("[skull] no pawn");
            return;
        }
        let mut bits = [0u8; 32];
        bits[value as usize / 8] = 1u8 << (value % 8);
        let fname = if on { "OnSkullsAdded" } else { "OnSkullsRemoved" };
        let ok = pe_call(pawn, fname, bits.as_mut_ptr() as *mut c_void, 32);
        crate::rep!("[skull] {}(bit {}) -> {}", fname, value, ok);
    });
    if !ran {
        crate::rep!("[skull] apply faulted");
    }
}

/// Freeze/resume the Blam simulation itself via `SetGameAndBlamPaused`. Unlike UE's
/// `Pause`, this reaches the Blam sim's own pause state (the sim runs on a separate
/// clock). With the free-cam flying, this is a true freeze-frame for machinima.
/// ParmsSize 9: an 8-byte param at offset 0 (zeroed = null / FName None) + bool at 8.
fn blam_pause(pc: *mut u8, on: bool) {
    let ran = crate::seh::guard(|| {
        let mut b = [0u8; 16];
        b[8] = on as u8;
        if pe_call(pc, "SetGameAndBlamPaused", b.as_mut_ptr() as *mut c_void, 9) {
            crate::rep!("[sim] SetGameAndBlamPaused({on}) called");
        }
    });
    if !ran {
        crate::rep!("[sim] blam pause faulted");
    }
}

/// Diagnostic: resolve the Blam sim's `game_time_globals` (via the cross-thread TLS
/// walk in `simtime`) and dump its head so we can (a) confirm resolution and (b) spot the
/// `game_speed` field (a float that reads 1.0 at normal speed).
fn diag_time(_pc: *mut u8) {
    let ran = crate::seh::guard(|| unsafe {
        crate::rep!("[diag] sim base = 0x{:x}", crate::mem::sim_base());
        let t = crate::simtime::resolve_now();
        if t == 0 {
            crate::rep!("[diag] game_time_globals NOT found on any thread");
            return;
        }
        let tick_rate = *((t + crate::simtime::GTG_TICK_RATE) as *const i16);
        let tick_len = *((t + crate::simtime::GTG_TICK_LENGTH) as *const f32);
        crate::rep!("[diag] game_time @ 0x{:x} tick_rate={} tick_length={:.5}", t, tick_rate, tick_len);
        for off in (0x00usize..0x48).step_by(4) {
            let iv = *((t + off) as *const i32);
            let fv = *((t + off) as *const f32);
            crate::rep!("[diag] T+0x{:02x}: i32={:>11} f32={:.5}", off, iv, fv);
        }
    });
    if !ran {
        crate::rep!("[diag] faulted");
    }
}

/// UGameplayStatics::SetGlobalTimeDilation with the current TD value. `world` is any
/// live UObject (the camera manager works as a world context).
pub fn apply_time(world: *mut u8) {
    unsafe {
        let gs = crate::ue::reflect::find_class("GameplayStatics");
        if gs.is_null() {
            return;
        }
        let f = find_function(gs, "SetGlobalTimeDilation");
        if f.is_null() {
            return;
        }
        let mut b = [0u8; 16];
        *(b.as_mut_ptr() as *mut *mut u8) = world;
        *((b.as_mut_ptr() as usize + 8) as *mut f32) = time_dilation();
        process_event(world, f, b.as_mut_ptr() as *mut c_void);
    }
}

/// Run a console command string via `UKismetSystemLibrary::ExecuteConsoleCommand`
/// (WorldContext, FString Command, SpecificPlayer). Used for `ImGui.ToggleInput` and to
/// test the `Blam.Skull.*` cvars (third-person / night vision / cheats).
fn run_console(pc: *mut u8, command: &str) {
    unsafe {
        let ks = crate::ue::reflect::find_class("KismetSystemLibrary");
        if ks.is_null() {
            crate::rep!("[console] KismetSystemLibrary not found");
            return;
        }
        let f = find_function(ks, "ExecuteConsoleCommand");
        if f.is_null() {
            crate::rep!("[console] ExecuteConsoleCommand not found");
            return;
        }
        // FString Command laid out inline in the parms: {data ptr, ArrayNum, ArrayMax}.
        let cmd: Vec<u16> = command.encode_utf16().chain(core::iter::once(0)).collect();
        let len = cmd.len() as i32;
        let mut b = [0u8; 64];
        let p = b.as_mut_ptr();
        *(p as *mut *mut u8) = pc; // WorldContextObject
        *(p.add(8) as *mut *const u16) = cmd.as_ptr(); // FString.Data
        *(p.add(0x10) as *mut i32) = len; // FString.ArrayNum (incl. null)
        *(p.add(0x14) as *mut i32) = len; // FString.ArrayMax
        *(p.add(0x18) as *mut *mut u8) = pc; // SpecificPlayer
        process_event(pc, f, b.as_mut_ptr() as *mut c_void);
        crate::rep!("[console] {}", command);
    }
}

fn camera_fade(out: bool) {
    unsafe {
        let cam = find_live_by_class("CameraManager");
        if cam.is_null() {
            return;
        }
        let f = find_function(class_of(cam), "StartCameraFade");
        if f.is_null() {
            return;
        }
        let mut b = [0u8; 64];
        let p = b.as_mut_ptr();
        *(p as *mut f32) = if out { 0.0 } else { 1.0 }; // FromAlpha
        *(p.add(4) as *mut f32) = if out { 1.0 } else { 0.0 }; // ToAlpha
        *(p.add(8) as *mut f32) = 1.0; // Duration
        *(p.add(0x18) as *mut f32) = 1.0; // FLinearColor.A (black)
        *(p.add(0x1D)) = if out { 1 } else { 0 }; // bHoldWhenFinished
        process_event(cam, f, b.as_mut_ptr() as *mut c_void);
        crate::rep!("[cine] fade {}", if out { "to black" } else { "from black" });
    }
}
