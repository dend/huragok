//! Native ImGui control panel. The plugin dispatches FImGuiDemo::DrawControls each
//! frame through a .rdata function pointer while the live ImGui context is current.
//! We swap that pointer to our own draw callback, render our panel with the native
//! ImGui entry points, then tail-call the original.
//!
//! Only the verified-safe entry points are used here (Begin/End/Text/Button/
//! TreeNodeBehavior). Richer widgets (Checkbox/Slider/ProgressBar) are added one at
//! a time after their ABI is confirmed, because a wrong ABI corrupts ImGui state.

use core::ffi::{c_char, c_void};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::cmd::{push, Cmd};
use crate::mem::{base, patch_ptr};
use crate::offsets::*;
use crate::state::cam;

#[repr(C)]
#[derive(Clone, Copy)]
struct ImVec2 {
    x: f32,
    y: f32,
}

type DemoFn = unsafe extern "system" fn(*mut c_void);
type BeginFn = unsafe extern "system" fn(*const c_char, *mut bool, i32) -> bool;
type EndFn = unsafe extern "system" fn();
type TextFn = unsafe extern "system" fn(*const c_char, *const c_char);
type ButtonFn = unsafe extern "system" fn(*const c_char, *const ImVec2) -> bool;
type TreeFn = unsafe extern "system" fn(u32, i32, *const c_char, *const c_char) -> bool;
type ProgressFn = unsafe extern "system" fn(f32, *const ImVec2, *const c_char);
type CheckboxFn = unsafe extern "system" fn(*const c_char, *mut bool) -> bool;
type SliderFn =
    unsafe extern "system" fn(*const c_char, *mut f32, f32, f32, *const c_char, i32) -> bool;

static ORIG_DEMO: AtomicUsize = AtomicUsize::new(0);
static HOOKED: AtomicBool = AtomicBool::new(false);
static PANEL: AtomicBool = AtomicBool::new(false);
static FAULTED: AtomicBool = AtomicBool::new(false);

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
            HOOKED.store(true, Ordering::Relaxed);
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
        let ok = crate::seh::guard(|| unsafe { draw_panel() });
        if !ok && !FAULTED.swap(true, Ordering::Relaxed) {
            crate::rep!("[panel] draw faulted (guarded)");
        }
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
    let progress: ProgressFn = core::mem::transmute(base() + IMGUI_PROGRESS_BAR);
    let checkbox: CheckboxFn = core::mem::transmute(base() + IMGUI_CHECKBOX);
    let slider: SliderFn = core::mem::transmute(base() + IMGUI_SLIDER_FLOAT);

    // Full width, auto height -> uniform buttons.
    static FULL: ImVec2 = ImVec2 { x: -1.0, y: 0.0 };
    const OPEN: i32 = 0x20;
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
            button($label.as_ptr() as *const c_char, &FULL)
        };
    }
    macro_rules! label {
        ($s:expr) => {
            text(b"%s\0".as_ptr() as *const c_char, $s.as_ptr() as *const c_char)
        };
    }
    macro_rules! line {
        ($($arg:tt)*) => {
            if let Ok(t) = std::ffi::CString::new(format!($($arg)*)) {
                text(b"%s\0".as_ptr() as *const c_char, t.as_ptr());
            }
        };
    }

    if !begin(b"Huragok\0".as_ptr() as *const c_char, core::ptr::null_mut(), 0) {
        end();
        return;
    }

    if header!(b"Stats\0", OPEN) {
        let (h, s, alive, total, valid) = crate::stats::snapshot();
        if valid {
            // ProgressBars, each guarded so a wrong ABI skips itself instead of
            // aborting the draw before End() (which would unbalance the window stack).
            let hp = std::ffi::CString::new(format!("Health {h:.0}%")).unwrap_or_default();
            let hf = if h.is_finite() { (h / 100.0).clamp(0.0, 1.0) } else { 0.0 };
            crate::seh::guard(|| progress(hf, &FULL, hp.as_ptr()));
            let sp = std::ffi::CString::new(format!("Shield {s:.0}%")).unwrap_or_default();
            let sf = if s.is_finite() { (s / 100.0).clamp(0.0, 1.0) } else { 0.0 };
            crate::seh::guard(|| progress(sf, &FULL, sp.as_ptr()));
            if total < 0 {
                label!(b"Enemies  counting...\0");
            } else {
                line!("Enemies  {alive} alive / {total} total");
            }
        } else {
            label!(b"waiting for pawn...\0");
        }
    }

    if header!(b"Machinima\0", OPEN) {
        // Free-cam checkbox (each widget guarded so a bad ABI skips itself).
        let mut fc = cam().freecam;
        let mut ch = false;
        crate::seh::guard(|| ch = checkbox(b"Free-cam\0".as_ptr() as *const c_char, &mut fc));
        if ch {
            let mut c = cam();
            c.freecam = fc;
            if fc {
                c.seed = true;
            }
            drop(c);
            push(if fc { Cmd::Freeze } else { Cmd::Unfreeze });
        }
        let mut mo = cam().mouse;
        let mut ch = false;
        crate::seh::guard(|| ch = checkbox(b"Mouse-look\0".as_ptr() as *const c_char, &mut mo));
        if ch {
            cam().mouse = mo;
        }
        let mut fov = cam().fov;
        let mut ch = false;
        crate::seh::guard(|| {
            ch = slider(
                b"FOV\0".as_ptr() as *const c_char,
                &mut fov,
                20.0,
                140.0,
                b"%.0f\0".as_ptr() as *const c_char,
                0,
            )
        });
        if ch {
            let mut c = cam();
            c.fov = fov;
            c.fov_locked = true;
        }
        let mut td = crate::pawn::time_dilation();
        let mut ch = false;
        crate::seh::guard(|| {
            ch = slider(
                b"Time\0".as_ptr() as *const c_char,
                &mut td,
                0.05,
                4.0,
                b"%.2f\0".as_ptr() as *const c_char,
                0,
            )
        });
        if ch {
            push(Cmd::Slomo(td));
        }
        if btn!(b"FOV reset\0") {
            let mut c = cam();
            c.fov = 90.0;
            c.fov_locked = false;
        }
        if btn!(b"Cinematic ON (hide HUD/player)\0") {
            push(Cmd::CineOn);
        }
        if btn!(b"Cinematic OFF\0") {
            push(Cmd::CineOff);
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
    }

    if header!(b"Pawn FX\0", 0) {
        if btn!(b"Active Camo ON\0") {
            push(Cmd::CamoOn);
        }
        if btn!(b"Active Camo OFF\0") {
            push(Cmd::CamoOff);
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

    end();
}
