//! Native ImGui control panel. The plugin dispatches FImGuiDemo::DrawControls each
//! frame through a .rdata function pointer while the live ImGui context is current.
//! We swap that pointer to our own draw callback, render our panel with the native
//! ImGui entry points, then tail-call the original.

use core::ffi::{c_char, c_void};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::cmd::{push, Cmd};
use crate::mem::{base, patch_ptr};
use crate::offsets::*;
use crate::state::cam;

type DemoFn = unsafe extern "system" fn(*mut c_void);
type BeginFn = unsafe extern "system" fn(*const c_char, *mut bool, i32) -> bool;
type EndFn = unsafe extern "system" fn();
// win64 has one calling convention, so a fixed 2-arg pointer safely calls variadic Text.
type TextFn = unsafe extern "system" fn(*const c_char, *const c_char);
type ButtonFn = unsafe extern "system" fn(*const c_char, *const f32) -> bool;
type TreeFn = unsafe extern "system" fn(u32, i32, *const c_char, *const c_char) -> bool;

static ORIG_DEMO: AtomicUsize = AtomicUsize::new(0);
static HOOKED: AtomicBool = AtomicBool::new(false);
static PANEL: AtomicBool = AtomicBool::new(false);

/// Swap the DrawControls dispatch pointer to our draw callback.
pub fn install() {
    if HOOKED.load(Ordering::Relaxed) {
        return;
    }
    unsafe {
        let slot = (base() + DRAWCONTROLS_SLOT) as *mut usize;
        let expect = base() + DRAWCONTROLS;
        let cur = *slot;
        crate::rep!("[demo] fnptr@{:p} = {:#x} (expect {:#x})", slot, cur, expect);
        if cur != expect {
            crate::rep!("[demo] MISMATCH - RVA off, not swapping");
            HOOKED.store(true, Ordering::Relaxed); // stop retrying
            return;
        }
        ORIG_DEMO.store(cur, Ordering::Relaxed);
        let detour: DemoFn = demo_hook;
        patch_ptr(slot, detour as usize);
        HOOKED.store(true, Ordering::Relaxed);
        crate::rep!("[demo] DrawControls hook installed. B toggles the panel.");
    }
}

/// Toggle the panel (installs the hook on first use).
pub fn toggle() {
    if !HOOKED.load(Ordering::Relaxed) {
        install();
    }
    let on = !PANEL.load(Ordering::Relaxed);
    PANEL.store(on, Ordering::Relaxed);
    crate::rep!("[panel] {}", if on { "ON" } else { "OFF" });
}

unsafe extern "system" fn demo_hook(self_: *mut c_void) {
    if PANEL.load(Ordering::Relaxed) {
        crate::seh::guard(|| unsafe { draw_panel() });
    }
    let orig: DemoFn = core::mem::transmute(ORIG_DEMO.load(Ordering::Relaxed));
    orig(self_);
}

#[allow(unused_assignments)] // the per-header id counter's final bump is intentional
unsafe fn draw_panel() {
    let begin: BeginFn = core::mem::transmute(base() + IMGUI_BEGIN);
    let end: EndFn = core::mem::transmute(base() + IMGUI_END);
    let text: TextFn = core::mem::transmute(base() + IMGUI_TEXT);
    let button: ButtonFn = core::mem::transmute(base() + IMGUI_BUTTON);
    let tree: TreeFn = core::mem::transmute(base() + IMGUI_TREENODE);

    static ZERO: [f32; 2] = [0.0, 0.0];
    const OPEN: i32 = 0x20; // ImGuiTreeNodeFlags_DefaultOpen
    let mut hid: u32 = 0xC0DE_0001;

    macro_rules! header {
        ($label:expr, $extra:expr) => {{
            let id = hid;
            hid = hid.wrapping_add(1);
            tree(id, 0x1A | $extra, $label.as_ptr() as *const c_char, core::ptr::null())
        }};
    }
    macro_rules! btn {
        ($label:expr) => {
            button($label.as_ptr() as *const c_char, ZERO.as_ptr())
        };
    }

    if begin(b"Huragok\0".as_ptr() as *const c_char, core::ptr::null_mut(), 0) {
        let (fc, mo, fov) = {
            let s = cam();
            (s.freecam, s.mouse, s.fov)
        };
        if let Ok(status) = std::ffi::CString::new(format!(
            "cam {}   mouse {}   fov {:.0}   time {:.2}   kf {}",
            if fc { "FREE" } else { "att" },
            if mo { "ON" } else { "off" },
            fov,
            crate::pawn::time_dilation(),
            crate::paths::count()
        )) {
            text(b"%s\0".as_ptr() as *const c_char, status.as_ptr());
        }

        if header!(b"Stats\0", OPEN) {
            let (h, s, valid) = crate::stats::snapshot();
            if valid {
                if let Ok(t) = std::ffi::CString::new(format!("Health   {h:.0}%")) {
                    text(b"%s\0".as_ptr() as *const c_char, t.as_ptr());
                }
                if let Ok(t) = std::ffi::CString::new(format!("Shield   {s:.0}%")) {
                    text(b"%s\0".as_ptr() as *const c_char, t.as_ptr());
                }
            } else {
                text(
                    b"%s\0".as_ptr() as *const c_char,
                    b"waiting for pawn...\0".as_ptr() as *const c_char,
                );
            }
        }

        if header!(b"Machinima\0", OPEN) {
            if btn!(b"Free-cam toggle\0") {
                let mut s = cam();
                s.freecam = !s.freecam;
                if s.freecam {
                    s.seed = true;
                }
                let on = s.freecam;
                drop(s);
                push(if on { Cmd::Freeze } else { Cmd::Unfreeze });
            }
            if btn!(b"Mouse-look toggle\0") {
                let mut s = cam();
                s.mouse = !s.mouse;
            }
            if btn!(b"FOV -\0") {
                let mut s = cam();
                s.fov = (s.fov - 5.0).max(20.0);
                s.fov_locked = true;
            }
            if btn!(b"FOV +\0") {
                let mut s = cam();
                s.fov = (s.fov + 5.0).min(140.0);
                s.fov_locked = true;
            }
            if btn!(b"FOV reset\0") {
                let mut s = cam();
                s.fov = 90.0;
                s.fov_locked = false; // stop forcing; restore the game's own FOV
            }
            if btn!(b"Cinematic ON (hide HUD/player)\0") {
                push(Cmd::CineOn);
            }
            if btn!(b"Cinematic OFF\0") {
                push(Cmd::CineOff);
            }
            if btn!(b"Time slower\0") {
                push(Cmd::Slomo((crate::pawn::time_dilation() - 0.1).max(0.05)));
            }
            if btn!(b"Time faster\0") {
                push(Cmd::Slomo((crate::pawn::time_dilation() + 0.1).min(4.0)));
            }
            if btn!(b"Time reset\0") {
                push(Cmd::Slomo(1.0));
            }
            if btn!(b"Pause toggle\0") {
                push(Cmd::Pause);
            }
            if btn!(b"Add Keyframe\0") {
                crate::paths::add();
            }
            if btn!(b"Play / Stop Path\0") {
                crate::paths::toggle_play();
            }
            if btn!(b"Clear Path\0") {
                crate::paths::clear();
            }
        }

        if header!(b"Character / Pawn\0", 0) {
            if btn!(b"Hide body\0") {
                push(Cmd::PawnHide);
            }
            if btn!(b"Show body\0") {
                push(Cmd::PawnShow);
            }
            if btn!(b"Collision OFF\0") {
                push(Cmd::PawnNoCol);
            }
            if btn!(b"Collision ON\0") {
                push(Cmd::PawnCol);
            }
            if btn!(b"Freeze pawn\0") {
                push(Cmd::FreezePawn);
            }
            if btn!(b"Unfreeze pawn\0") {
                push(Cmd::UnfreezePawn);
            }
            if btn!(b"Giant\0") {
                push(Cmd::ScaleGiant);
            }
            if btn!(b"Tiny\0") {
                push(Cmd::ScaleTiny);
            }
            if btn!(b"Normal size\0") {
                push(Cmd::ScaleNormal);
            }
            if btn!(b"Teleport to camera\0") {
                push(Cmd::Teleport);
            }
            if btn!(b"Show full body (free-cam)\0") {
                push(Cmd::FullbodyOn);
            }
            if btn!(b"Restore first-person\0") {
                push(Cmd::FullbodyOff);
            }
        }

        if header!(b"Pawn FX\0", 0) {
            if btn!(b"Active Camo ON\0") {
                push(Cmd::CamoOn);
            }
            if btn!(b"Active Camo OFF\0") {
                push(Cmd::CamoOff);
            }
            if btn!(b"Camo toggle\0") {
                push(Cmd::CamoToggle);
            }
            if btn!(b"Overshield ON\0") {
                push(Cmd::OvershieldOn);
            }
            if btn!(b"Overshield OFF\0") {
                push(Cmd::OvershieldOff);
            }
            if btn!(b"Shield-break FX\0") {
                push(Cmd::ShieldBreak);
            }
            if btn!(b"Recharge FX\0") {
                push(Cmd::RechargePP);
            }
            if btn!(b"Night vision\0") {
                push(Cmd::NightVis);
            }
            if btn!(b"Radial blur\0") {
                push(Cmd::RadialBlur);
            }
            if btn!(b"Breath fog\0") {
                push(Cmd::Breath);
            }
            if btn!(b"Blood: Human\0") {
                push(Cmd::BloodHuman);
            }
            if btn!(b"Blood: Covenant\0") {
                push(Cmd::BloodCov);
            }
            if btn!(b"Blood: Grunt\0") {
                push(Cmd::BloodGrunt);
            }
            if btn!(b"Blood: Brute\0") {
                push(Cmd::BloodBrute);
            }
        }

        if header!(b"UI\0", 0) {
            if btn!(b"Toggle ImGui Input (cursor)\0") {
                push(Cmd::ImguiInput);
            }
        }
    }
    end();
}
