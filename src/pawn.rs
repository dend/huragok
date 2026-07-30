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
            Cmd::FullbodyOn | Cmd::FullbodyOff => {
                // Disabled: the world-rep offsets (pawn+0x3F8 etc.) shifted on this build,
                // so driving the updater hangs the game. Needs re-derivation before re-enabling.
                FULLBODY.store(false, Ordering::Relaxed);
                crate::rep!("[fullbody] disabled on this build (offsets shifted, needs re-RE)");
            }
            Cmd::ImguiInput => imgui_toggle_input(pc),
            Cmd::Slomo(v) => {
                set_time_dilation(v);
                let gs = crate::ue::reflect::find_class("GameplayStatics");
                let f = if gs.is_null() {
                    core::ptr::null_mut()
                } else {
                    find_function(gs, "SetGlobalTimeDilation")
                };
                if f.is_null() {
                    crate::rep!("[time] SetGlobalTimeDilation NOT found (gs={:p}) - not applied", gs);
                } else {
                    apply_time(pc);
                    crate::rep!("[time] SetGlobalTimeDilation({:.2}) called; if no slow-mo the game ignores UE time dilation", v);
                }
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

fn imgui_toggle_input(pc: *mut u8) {
    unsafe {
        let ks = crate::ue::reflect::find_class("KismetSystemLibrary");
        if ks.is_null() {
            return;
        }
        let f = find_function(ks, "ExecuteConsoleCommand");
        if f.is_null() {
            return;
        }
        let cmd: Vec<u16> = "ImGui.ToggleInput"
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        let len = cmd.len() as i32;
        let mut b = [0u8; 64];
        let p = b.as_mut_ptr();
        *(p as *mut *mut u8) = pc;
        *(p.add(8) as *mut *const u16) = cmd.as_ptr();
        *(p.add(0x10) as *mut i32) = len;
        *(p.add(0x14) as *mut i32) = len;
        *(p.add(0x18) as *mut *mut u8) = pc;
        process_event(pc, f, b.as_mut_ptr() as *mut c_void);
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
