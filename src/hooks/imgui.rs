//! Native ImGui control panel. The plugin dispatches FImGuiDemo::DrawControls each
//! frame through a .rdata function pointer while the live ImGui context is current.
//! We swap that pointer to our own draw callback, render our panel, then tail-call
//! the original.
//!
//! Every non-trivial widget call is wrapped in `guarded(label, ...)`: a fault is
//! caught (so it can never unbalance the ImGui window stack and hang the game) and
//! logged once with its label, so a wrong RVA/ABI is diagnosable from the log.

use core::ffi::{c_char, c_void};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

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
// 0x73191f0 is really ImGui::SliderScalar: (label, data_type, p_data, p_min, p_max, fmt, flags).
// data_type 8 = ImGuiDataType_Float; min/max are pointers to the pointee type (f32 here).
type SliderFn = unsafe extern "system" fn(
    *const c_char,
    i32,
    *mut c_void,
    *const c_void,
    *const c_void,
    *const c_char,
    i32,
) -> bool;
const IMGUI_DATATYPE_FLOAT: i32 = 8;
type InvisibleFn = unsafe extern "system" fn(*const c_char, *const ImVec2, i32) -> bool;
type AddLineFn = unsafe extern "system" fn(*mut u8, *const ImVec2, *const ImVec2, u32, f32);
type AddRectFn = unsafe extern "system" fn(*mut u8, *const ImVec2, *const ImVec2, u32, f32, i32);
type AddCircleFn = unsafe extern "system" fn(*mut u8, *const ImVec2, f32, u32, i32);

static ORIG_DEMO: AtomicUsize = AtomicUsize::new(0);
static HOOKED: AtomicBool = AtomicBool::new(false);
static PANEL: AtomicBool = AtomicBool::new(false);
static FAULTED_WIDGETS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// Run a widget call under SEH. On fault, skip it and log the label once.
fn guarded(label: &'static str, f: impl FnMut()) {
    if crate::seh::guard(f) {
        return;
    }
    if let Ok(mut v) = FAULTED_WIDGETS.lock() {
        if !v.contains(&label) {
            v.push(label);
            crate::rep!("[panel] widget '{label}' faulted (guarded); skipping it");
        }
    }
}

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
        // Backstop guard around the whole draw, on top of the per-widget guards.
        guarded("panel", || unsafe { draw_panel() });
    }
    let orig: DemoFn = core::mem::transmute(ORIG_DEMO.load(Ordering::Relaxed));
    orig(self_);
}

#[allow(unused_assignments)]
unsafe fn draw_panel() {
    let begin: BeginFn = core::mem::transmute(base() + IMGUI_BEGIN);
    let end: EndFn = core::mem::transmute(base() + IMGUI_END);
    let text: TextFn = core::mem::transmute(base() + IMGUI_TEXT);
    let button: ButtonFn = core::mem::transmute(base() + IMGUI_BUTTON);
    let tree: TreeFn = core::mem::transmute(base() + IMGUI_TREENODE);
    let progress: ProgressFn = core::mem::transmute(base() + IMGUI_PROGRESS_BAR);
    let checkbox: CheckboxFn = core::mem::transmute(base() + IMGUI_CHECKBOX);
    let slider: SliderFn = core::mem::transmute(base() + IMGUI_SLIDER_FLOAT);

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

    // ---- Stats ----
    if header!(b"Stats\0", OPEN) {
        let (h, s, alive, total, valid) = crate::stats::snapshot();
        if valid {
            let hp = std::ffi::CString::new(format!("Health {h:.0}%")).unwrap_or_default();
            let hf = if h.is_finite() { (h / 100.0).clamp(0.0, 1.0) } else { 0.0 };
            guarded("health bar", || progress(hf, &FULL, hp.as_ptr()));
            let sp = std::ffi::CString::new(format!("Shield {s:.0}%")).unwrap_or_default();
            let sf = if s.is_finite() { (s / 100.0).clamp(0.0, 1.0) } else { 0.0 };
            guarded("shield bar", || progress(sf, &FULL, sp.as_ptr()));
            if total < 0 {
                label!(b"Enemies  counting...\0");
            } else {
                line!("Enemies  {alive} alive / {total} total");
            }
        } else {
            label!(b"waiting for pawn...\0");
        }
    }

    // ---- Machinima ----
    if header!(b"Machinima\0", OPEN) {
        let mut fc = cam().freecam;
        let mut ch = false;
        guarded("free-cam checkbox", || {
            ch = checkbox(b"Free-cam\0".as_ptr() as *const c_char, &mut fc)
        });
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
        guarded("mouse checkbox", || {
            ch = checkbox(b"Mouse-look\0".as_ptr() as *const c_char, &mut mo)
        });
        if ch {
            cam().mouse = mo;
        }
        let mut fov = cam().fov;
        let (fmin, fmax) = (20.0f32, 140.0f32);
        let mut ch = false;
        guarded("fov slider", || {
            ch = slider(
                b"FOV\0".as_ptr() as *const c_char,
                IMGUI_DATATYPE_FLOAT,
                &mut fov as *mut f32 as *mut c_void,
                &fmin as *const f32 as *const c_void,
                &fmax as *const f32 as *const c_void,
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
        let (tmin, tmax) = (0.05f32, 4.0f32);
        let mut ch = false;
        guarded("time slider", || {
            ch = slider(
                b"Time\0".as_ptr() as *const c_char,
                IMGUI_DATATYPE_FLOAT,
                &mut td as *mut f32 as *mut c_void,
                &tmin as *const f32 as *const c_void,
                &tmax as *const f32 as *const c_void,
                b"%.2f\0".as_ptr() as *const c_char,
                0,
            )
        });
        if ch {
            push(Cmd::Slomo(td));
        }
        if btn!(b"Time reset (1x)\0") {
            push(Cmd::Slomo(1.0));
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
        if btn!(b"Freeze sim (Blam)\0") {
            push(Cmd::SimFreeze);
        }
        if btn!(b"Resume sim (Blam)\0") {
            push(Cmd::SimUnfreeze);
        }
        if btn!(b"Diag: time timer\0") {
            push(Cmd::DiagTime);
        }
    }

    // ---- Timeline ----
    if header!(b"Timeline\0", OPEN) {
        guarded("timeline", || unsafe { draw_timeline() });
        let (count, _ph, playing) = crate::paths::timeline();
        line!(
            "{} keyframes   {}",
            count,
            if playing { "playing" } else { "stopped" }
        );
        if btn!(b"Add Keyframe\0") {
            crate::paths::add();
        }
        if btn!(b"Play / Stop\0") {
            crate::paths::toggle_play();
        }
        if btn!(b"Clear\0") {
            crate::paths::clear();
        }
    }

    // ---- Character / Pawn ----
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
        if btn!(b"Show full body\0") {
            push(Cmd::FullbodyOn);
        }
        if btn!(b"Hide body (first-person)\0") {
            push(Cmd::FullbodyOff);
        }
    }

    // ---- Pawn FX ----
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

    // ---- UI ----
    if header!(b"UI\0", 0) {
        if btn!(b"Toggle ImGui Input (cursor)\0") {
            push(Cmd::ImguiInput);
        }
    }

    end();
}

/// Draw the keyframe track: a bar, a dot per keyframe, and the playhead line.
/// Uses the window draw list (resolved through inlined ImGui offsets).
unsafe fn draw_timeline() {
    let inv: InvisibleFn = core::mem::transmute(base() + IMGUI_INVISIBLE_BUTTON);
    let add_rect: AddRectFn = core::mem::transmute(base() + IMGUI_DRAW_ADD_RECT_FILLED);
    let add_line: AddLineFn = core::mem::transmute(base() + IMGUI_DRAW_ADD_LINE);
    let add_circle: AddCircleFn = core::mem::transmute(base() + IMGUI_DRAW_ADD_CIRCLE_FILLED);

    let g = *((base() + GIMGUI_PTR) as *const usize);
    if g == 0 {
        return;
    }
    let win = *((g + IMGUI_CTX_CURRENT_WINDOW) as *const usize);
    if win == 0 {
        return;
    }
    let dl = *((win + IMGUI_WIN_DRAWLIST) as *const usize) as *mut u8;
    if dl.is_null() {
        return;
    }

    // Reserve the track area so following widgets do not overlap it.
    let origin = *((win + IMGUI_WIN_CURSOR_POS) as *const ImVec2);
    let size = ImVec2 { x: 320.0, y: 46.0 };
    inv(b"##timeline\0".as_ptr() as *const c_char, &size, 0);

    let x0 = origin.x + 4.0;
    let y0 = origin.y + 6.0;
    let w = size.x - 8.0;
    let h = 32.0;

    // Colours are 0xAABBGGRR.
    add_rect(dl, &ImVec2 { x: x0, y: y0 }, &ImVec2 { x: x0 + w, y: y0 + h }, 0xff2a_2a2a, 4.0, 0);

    let (count, playhead, _playing) = crate::paths::timeline();
    let cy = y0 + h * 0.5;
    for i in 0..count {
        let f = if count > 1 {
            i as f32 / (count - 1) as f32
        } else {
            0.0
        };
        let cx = x0 + f * w;
        add_circle(dl, &ImVec2 { x: cx, y: cy }, 5.0, 0xff50_c8ff, 12);
    }
    if count >= 1 {
        let px = x0 + playhead as f32 * w;
        add_line(dl, &ImVec2 { x: px, y: y0 }, &ImVec2 { x: px, y: y0 + h }, 0xff50_50ff, 2.0);
    }
}
