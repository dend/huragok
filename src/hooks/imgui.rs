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
// InputText(label, buf, buf_size, flags, callback, user_data) -> bool.
type InputTextFn =
    unsafe extern "system" fn(*const c_char, *mut c_char, usize, i32, *mut c_void, *mut c_void) -> bool;
const IMGUI_INPUT_ENTER_RETURNS_TRUE: i32 = 0x20;
type BeginChildFn = unsafe extern "system" fn(*const c_char, *const ImVec2, i32, i32) -> bool;
type EndChildFn = unsafe extern "system" fn();
type SetScrollHereYFn = unsafe extern "system" fn(f32);

static ORIG_DEMO: AtomicUsize = AtomicUsize::new(0);
static HOOKED: AtomicBool = AtomicBool::new(false);
static PANEL: AtomicBool = AtomicBool::new(false);
static SHOW_KF: AtomicBool = AtomicBool::new(false); // keyframe list window open
static SHOW_CONSOLE: AtomicBool = AtomicBool::new(false); // ImGui console window open
static FLASHLIGHT: AtomicBool = AtomicBool::new(false); // flashlight checkbox state
static FAULTED_WIDGETS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

// Input buffer for the ImGui console box. Only touched on the ImGui draw thread.
static mut CMD_BUF: [u8; 256] = [0u8; 256];

// Skulls shown as checkboxes: (label with explainer, hs skull name). Toggling one runs
// `hs:skull_enable <name> true|false`. State is what the user last set (the game does not
// report skull state back cheaply).
// Every gameplay skull, generated from the simulation module (name, hs name).
const SKULLS: &[(&str, &str)] = &[
    ("Third Person", "skull_third_person"),
    ("Night Vision", "skull_night_vision"),
    ("Bandanna", "skull_bandanna"),
    ("Adaptation", "skull_adaptation"),
    ("Angry", "skull_angry"),
    ("Armistice", "skull_armistice"),
    ("Assassin", "skull_assassin"),
    ("Birthday Party", "skull_birthday_party"),
    ("Black Eye", "skull_black_eye"),
    ("Blind", "skull_blind"),
    ("Bonded Pair", "skull_bonded_pair"),
    ("Boom", "skull_boom"),
    ("Boots Off The Ground", "skull_boots_off_the_ground"),
    ("Catch", "skull_catch"),
    ("Efficient", "skull_efficient"),
    ("Eye Patch", "skull_eye_patch"),
    ("Famine", "skull_famine"),
    ("Floor Is Lava", "skull_floor_is_lava"),
    ("Fog", "skull_fog"),
    ("Foreign", "skull_foreign"),
    ("Fragile", "skull_fragile"),
    ("Ghost", "skull_ghost"),
    ("Give And Take", "skull_give_and_take"),
    ("Grunt Birthday Party", "skull_grunt_birthday_party"),
    ("Grunt Funeral", "skull_grunt_funeral"),
    ("Hip Fire", "skull_hip_fire"),
    ("Iron", "skull_iron"),
    ("IWHBYD", "skull_iwhbyd"),
    ("Jacked", "skull_jacked"),
    ("Johnny Ammo Tree", "skull_johnny_ammo_tree"),
    ("Leadhead", "skull_leadhead"),
    ("Lights Out", "skull_lights_out"),
    ("Magnified", "skull_magnified"),
    ("Malfunction", "skull_malfunction"),
    ("Masterblaster", "skull_masterblaster"),
    ("Mythic", "skull_mythic"),
    ("Pinata", "skull_pinata"),
    ("Pop", "skull_pop"),
    ("Recession", "skull_recession"),
    ("Reload", "skull_reload"),
    ("Riskrun", "skull_riskrun"),
    ("Scarab", "skull_scarab"),
    ("So Angry", "skull_so_angry"),
    ("Spore Visibility", "skull_spore_visibility"),
    ("Stow And Grow", "skull_stow_and_grow"),
    ("Superman", "skull_superman"),
    ("Swarm", "skull_swarm"),
    ("Temperamental", "skull_temperamental"),
    ("That's Just Wrong", "skull_thats_just_wrong"),
    ("They Come Back", "skull_they_come_back"),
    ("Thunderstorm", "skull_thunderstorm"),
    ("Tilt", "skull_tilt"),
    ("Tough Luck", "skull_tough_luck"),
];
static SKULL_STATE: [AtomicBool; SKULLS.len()] = [const { AtomicBool::new(false) }; SKULLS.len()];

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
    crate::perf::tick(); // read GAverageFPS each frame (plain global read, thread-safe)
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
        line!("FPS  {:.0}", crate::perf::current());
        guarded("fps graph", || unsafe { draw_fps_graph() });
    }

    // ---- Campaign / Mission ----
    if header!(b"Campaign\0", OPEN) {
        let (level, diff, cp, seg) = crate::campaign::snapshot();
        if level.is_empty() {
            label!(b"mission: (loading)\0");
        } else {
            line!("Mission: {}", level);
        }
        if !diff.is_empty() {
            line!("Difficulty: {}", diff);
        }
        if cp >= 0 {
            line!("Checkpoint: {}", cp);
        }
        if seg >= 0 {
            line!("Segment: {}", seg);
        }
        if btn!(b"Diag: mission\0") {
            push(Cmd::DiagMission);
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
        line!("{} keyframes   {}", count, if playing { "playing" } else { "stopped" });
        if btn!(b"Add Keyframe\0") {
            crate::paths::add();
        }
        if btn!(b"Play / Stop\0") {
            crate::paths::toggle_play();
        }
        if btn!(b"Clear\0") {
            crate::paths::clear();
        }
        let kf_open = SHOW_KF.load(Ordering::Relaxed);
        if btn!(if kf_open { b"Hide keyframe list\0" } else { b"Show keyframe list\0" }) {
            SHOW_KF.store(!kf_open, Ordering::Relaxed);
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
        // Flashlight checkbox (off may be a no-op on this build - it is enable-latched like
        // the skulls). Third/first-person are managed by the Third Person skull.
        let mut fl = FLASHLIGHT.load(Ordering::Relaxed);
        let mut ch = false;
        guarded("flashlight", || {
            ch = checkbox(b"Flashlight\0".as_ptr() as *const c_char, &mut fl)
        });
        if ch {
            FLASHLIGHT.store(fl, Ordering::Relaxed);
            crate::console::submit(format!(
                "hs:unit_set_integrated_flashlight (player0) {}",
                if fl { "true" } else { "false" }
            ));
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

    // ---- Skulls: checkboxes, one per skull (hs:skull_enable). Any skull not listed can
    //      still be typed into the console: hs:skull_enable skull_<name> true
    if header!(b"Skulls\0", 0) {
        for (i, (label, name)) in SKULLS.iter().enumerate() {
            let mut on = SKULL_STATE[i].load(Ordering::Relaxed);
            let mut changed = false;
            if let Ok(l) = std::ffi::CString::new(*label) {
                guarded("skull checkbox", || changed = checkbox(l.as_ptr(), &mut on));
            }
            if changed {
                SKULL_STATE[i].store(on, Ordering::Relaxed);
                // Night vision is a render-gated skull (bit 40) read live from the sim skull
                // set, so we toggle it directly - HS `skull_enable false` fails to disable it.
                // Everything else still goes through hs:skull_enable.
                if *name == "skull_night_vision" {
                    push(Cmd::NightVision(on));
                } else {
                    crate::console::submit(format!(
                        "hs:skull_enable {} {}",
                        name,
                        if on { "true" } else { "false" }
                    ));
                }
            }
        }
    }

    // ---- UI ----
    if header!(b"UI\0", 0) {
        if btn!(b"Toggle ImGui Input (cursor)\0") {
            push(Cmd::ImguiInput);
        }
        let con_open = SHOW_CONSOLE.load(Ordering::Relaxed);
        if btn!(if con_open { b"Hide console\0" } else { b"Show console\0" }) {
            SHOW_CONSOLE.store(!con_open, Ordering::Relaxed);
        }
    }

    end();

    // Separate keyframe list window, toggled from the Timeline section.
    if SHOW_KF.load(Ordering::Relaxed) {
        guarded("kf window", || unsafe { draw_keyframe_window() });
    }
    // Separate console window (log tail + command input box).
    if SHOW_CONSOLE.load(Ordering::Relaxed) {
        guarded("console window", || unsafe { draw_console_window() });
    }
}

/// In-game console: a tail of recent log lines plus an input box. Type a command (e.g.
/// `hs:skull_enable skull_third_person true`) and press Enter to run it via
/// ExecuteConsoleCommand. Needs ImGui input capture on (the "Toggle ImGui Input" button).
unsafe fn draw_console_window() {
    let begin: BeginFn = core::mem::transmute(base() + IMGUI_BEGIN);
    let end: EndFn = core::mem::transmute(base() + IMGUI_END);
    let text: TextFn = core::mem::transmute(base() + IMGUI_TEXT);
    let input: InputTextFn = core::mem::transmute(base() + IMGUI_INPUT_TEXT);
    let begin_child: BeginChildFn = core::mem::transmute(base() + IMGUI_BEGIN_CHILD);
    let end_child: EndChildFn = core::mem::transmute(base() + IMGUI_END_CHILD);
    let set_scroll: SetScrollHereYFn = core::mem::transmute(base() + IMGUI_SET_SCROLL_HERE_Y);

    let mut open = true;
    if begin(b"Huragok Console\0".as_ptr() as *const c_char, &mut open as *mut bool, 0) {
        // Scrollable log region that fills the window minus a footer for the input line.
        let log_size = ImVec2 { x: 0.0, y: -28.0 };
        if begin_child(b"##log\0".as_ptr() as *const c_char, &log_size, 1, 0) {
            let mut lines: Vec<String> = Vec::new();
            crate::log::recent(&mut lines, 200);
            for l in &lines {
                if let Ok(t) = std::ffi::CString::new(l.as_str()) {
                    text(b"%s\0".as_ptr() as *const c_char, t.as_ptr());
                }
            }
            // Auto-follow only when already at the bottom, so scrolling up stays put.
            let g = *((base() + GIMGUI_PTR) as *const usize);
            if g != 0 {
                let cw = *((g + IMGUI_CTX_CURRENT_WINDOW) as *const usize);
                if cw != 0 {
                    let sy = *((cw + IMGUI_WIN_SCROLL_Y) as *const f32);
                    let smax = *((cw + IMGUI_WIN_SCROLLMAX_Y) as *const f32);
                    if sy >= smax - 1.0 {
                        set_scroll(1.0);
                    }
                }
            }
        }
        end_child();

        // Input box, pinned below the scroll region.
        let buf = core::ptr::addr_of_mut!(CMD_BUF) as *mut c_char;
        let submitted = input(
            b"##cmd\0".as_ptr() as *const c_char,
            buf,
            256,
            IMGUI_INPUT_ENTER_RETURNS_TRUE,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        if submitted {
            let s = std::ffi::CStr::from_ptr(buf).to_string_lossy().trim().to_string();
            if !s.is_empty() {
                crate::console::submit(s);
            }
            *buf = 0; // clear the box
        }
    }
    end();
    if !open {
        SHOW_CONSOLE.store(false, Ordering::Relaxed);
    }
}

/// A sparkline of recent FPS with a 60 FPS reference line. Uses the window draw list.
unsafe fn draw_fps_graph() {
    let inv: InvisibleFn = core::mem::transmute(base() + IMGUI_INVISIBLE_BUTTON);
    let add_rect: AddRectFn = core::mem::transmute(base() + IMGUI_DRAW_ADD_RECT_FILLED);
    let add_line: AddLineFn = core::mem::transmute(base() + IMGUI_DRAW_ADD_LINE);

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

    let origin = *((win + IMGUI_WIN_CURSOR_POS) as *const ImVec2);
    let size = ImVec2 { x: 320.0, y: 40.0 };
    inv(b"##fps\0".as_ptr() as *const c_char, &size, 0);

    let x0 = origin.x + 4.0;
    let y0 = origin.y + 4.0;
    let w = size.x - 8.0;
    let h = size.y - 8.0;

    // Colours are 0xAABBGGRR.
    add_rect(dl, &ImVec2 { x: x0, y: y0 }, &ImVec2 { x: x0 + w, y: y0 + h }, 0xff20_2020, 4.0, 0);

    let mut buf = [0f32; crate::perf::SAMPLES];
    let n = crate::perf::samples(&mut buf);
    if n < 2 {
        return;
    }
    // Scale the y axis to whichever is larger: 120 FPS or the current peak (capped).
    let mut peak = 120.0f32;
    for &v in buf.iter().take(n) {
        if v > peak {
            peak = v;
        }
    }
    peak = peak.min(360.0);

    // 60 FPS reference line (dim).
    let ref_y = y0 + h - (60.0 / peak).clamp(0.0, 1.0) * h;
    add_line(dl, &ImVec2 { x: x0, y: ref_y }, &ImVec2 { x: x0 + w, y: ref_y }, 0xff40_4040, 1.0);

    // The FPS trace (green).
    let pt = |i: usize| -> ImVec2 {
        let fx = i as f32 / (n - 1) as f32;
        let fy = (buf[i] / peak).clamp(0.0, 1.0);
        ImVec2 { x: x0 + fx * w, y: y0 + h - fy * h }
    };
    for i in 1..n {
        let a = pt(i - 1);
        let b = pt(i);
        add_line(dl, &a, &b, 0xff5c_c85c, 1.5);
    }
}

/// A separate window listing every keyframe with its captured coordinates, so the
/// timeline dots are legible. Toggled from the Timeline section.
unsafe fn draw_keyframe_window() {
    let begin: BeginFn = core::mem::transmute(base() + IMGUI_BEGIN);
    let end: EndFn = core::mem::transmute(base() + IMGUI_END);
    let text: TextFn = core::mem::transmute(base() + IMGUI_TEXT);

    let mut open = true;
    if begin(b"Keyframes\0".as_ptr() as *const c_char, &mut open as *mut bool, 0) {
        let kfs = crate::paths::keyframes();
        if kfs.is_empty() {
            text(
                b"%s\0".as_ptr() as *const c_char,
                b"No keyframes yet - use Add Keyframe.\0".as_ptr() as *const c_char,
            );
        } else {
            for (i, k) in kfs.iter().enumerate() {
                if let Ok(t) = std::ffi::CString::new(format!(
                    "#{:<2}  pos ({:.0}, {:.0}, {:.0})   yaw {:.0}  pitch {:.0}   fov {:.0}",
                    i + 1,
                    k.0,
                    k.1,
                    k.2,
                    k.4,
                    k.3,
                    k.6
                )) {
                    text(b"%s\0".as_ptr() as *const c_char, t.as_ptr());
                }
            }
        }
    }
    end();
    // Reflect the window's own close button back into our toggle.
    if !open {
        SHOW_KF.store(false, Ordering::Relaxed);
    }
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

    // Reserve the track area so following widgets do not overlap it. Span the full content
    // width: content-region max x (window+0x250) minus the cursor x.
    let origin = *((win + IMGUI_WIN_CURSOR_POS) as *const ImVec2);
    let content_max_x = *((win + IMGUI_WIN_CONTENT_MAX_X) as *const f32);
    let avail = (content_max_x - origin.x).max(64.0);
    let size = ImVec2 { x: avail, y: 46.0 };
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
