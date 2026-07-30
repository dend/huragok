//! Keyboard/mouse polling and the free-cam movement loop.

use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SetCursorPos, SM_CXSCREEN, SM_CYSCREEN,
};

use crate::state::cam;

// Virtual-key codes (raw, so we don't juggle VIRTUAL_KEY <-> i32).
const VK_INSERT: i32 = 0x2D;
const VK_SPACE: i32 = 0x20;
const VK_LSHIFT: i32 = 0xA0;
const VK_RSHIFT: i32 = 0xA1;
const VK_LCTRL: i32 = 0xA2;
const VK_RCTRL: i32 = 0xA3;
const VK_UP: i32 = 0x26;
const VK_DOWN: i32 = 0x28;
const VK_LEFT: i32 = 0x25;
const VK_RIGHT: i32 = 0x27;
const VK_OEM_4: i32 = 0xDB; // [
const VK_OEM_6: i32 = 0xDD; // ]
const K_W: i32 = b'W' as i32;
const K_A: i32 = b'A' as i32;
const K_S: i32 = b'S' as i32;
const K_D: i32 = b'D' as i32;
const K_E: i32 = b'E' as i32;
const K_Q: i32 = b'Q' as i32;
const K_Z: i32 = b'Z' as i32;
const K_C: i32 = b'C' as i32;
const K_X: i32 = b'X' as i32;
const K_M: i32 = b'M' as i32;
const K_B: i32 = b'B' as i32;
const K_P: i32 = b'P' as i32;
const K_I: i32 = b'I' as i32;
const VK_HOME: i32 = 0x24;
const VK_END: i32 = 0x23;
const VK_F5: i32 = 0x74;
const VK_F6: i32 = 0x75;
const VK_OEM_COMMA: i32 = 0xBC; // ,
const VK_OEM_PERIOD: i32 = 0xBE; // .
const VK_OEM_2: i32 = 0xBF; // /
const K_K: i32 = b'K' as i32;
const K_J: i32 = b'J' as i32;
const K_L: i32 = b'L' as i32;

const DEG: f64 = std::f64::consts::PI / 180.0;

/// Rising edge: true once per physical key press.
pub fn edge(vk: i32) -> bool {
    unsafe { (GetAsyncKeyState(vk) & 1) != 0 }
}

/// True while the key is currently held.
pub fn held(vk: i32) -> bool {
    unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
}

/// Poll non-camera hotkeys that push commands to the game thread. Call ~60 Hz.
pub fn poll_hotkeys() {
    use crate::cmd::{push, Cmd};
    if edge(K_B) {
        crate::hooks::imgui::toggle();
    }
    if edge(K_P) {
        push(Cmd::Pause);
    }
    if edge(K_I) {
        push(Cmd::ImguiInput);
    }
    if edge(VK_HOME) {
        push(Cmd::CineOn);
    }
    if edge(VK_END) {
        push(Cmd::CineOff);
    }
    if edge(VK_F5) {
        push(Cmd::FadeOut);
    }
    if edge(VK_F6) {
        push(Cmd::FadeIn);
    }
    if edge(VK_OEM_COMMA) {
        push(Cmd::Slomo((crate::pawn::time_dilation() - 0.1).max(0.05)));
    }
    if edge(VK_OEM_PERIOD) {
        push(Cmd::Slomo((crate::pawn::time_dilation() + 0.1).min(4.0)));
    }
    if edge(VK_OEM_2) {
        push(Cmd::Slomo(1.0));
    }
    if edge(K_K) {
        crate::paths::add();
    }
    if edge(K_J) {
        crate::paths::toggle_play();
    }
    if edge(K_L) {
        crate::paths::clear();
    }
}

/// Poll INSERT (toggle free-cam) and, while active, fly the camera. Call ~60 Hz.
pub fn poll_freecam() {
    if edge(VK_INSERT) {
        let mut s = cam();
        s.freecam = !s.freecam;
        if s.freecam {
            s.seed = true;
        }
        let on = s.freecam;
        drop(s);
        crate::rep!("[freecam] {}", if on { "ON" } else { "OFF" });
        // TODO(port): CMD_FREEZE / CMD_UNFREEZE so WASD stops driving the pawn too.
    }

    if edge(K_M) {
        let mut s = cam();
        s.mouse = !s.mouse;
        let on = s.mouse;
        drop(s);
        crate::rep!("[freecam] mouse-look {}", if on { "ON" } else { "OFF" });
    }

    let mut s = cam();
    if !s.freecam {
        return;
    }

    let fast = held(VK_LSHIFT) || held(VK_RSHIFT);
    let spd = if fast { 55.0 } else { 14.0 };
    let look = 1.6;

    let ry = s.yaw * DEG;
    let rp = s.pitch * DEG;
    let (fx, fy, fz) = (rp.cos() * ry.cos(), rp.cos() * ry.sin(), rp.sin());
    let (rxv, ryv) = (-ry.sin(), ry.cos());

    if held(K_W) {
        s.x += fx * spd;
        s.y += fy * spd;
        s.z += fz * spd;
    }
    if held(K_S) {
        s.x -= fx * spd;
        s.y -= fy * spd;
        s.z -= fz * spd;
    }
    if held(K_D) {
        s.x += rxv * spd;
        s.y += ryv * spd;
    }
    if held(K_A) {
        s.x -= rxv * spd;
        s.y -= ryv * spd;
    }
    if held(K_E) || held(VK_SPACE) {
        s.z += spd;
    }
    if held(K_Q) || held(VK_LCTRL) || held(VK_RCTRL) {
        s.z -= spd;
    }
    if held(VK_UP) {
        s.pitch = (s.pitch + look).min(89.0);
    }
    if held(VK_DOWN) {
        s.pitch = (s.pitch - look).max(-89.0);
    }
    if held(VK_LEFT) {
        s.yaw -= look;
    }
    if held(VK_RIGHT) {
        s.yaw += look;
    }

    if s.mouse {
        unsafe {
            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            let cx = GetSystemMetrics(SM_CXSCREEN) / 2;
            let cy = GetSystemMetrics(SM_CYSCREEN) / 2;
            s.yaw += (pt.x - cx) as f64 * 0.06;
            s.pitch = (s.pitch - (pt.y - cy) as f64 * 0.06).clamp(-89.0, 89.0);
            SetCursorPos(cx, cy);
        }
    }

    if held(VK_OEM_4) {
        s.fov = (s.fov - 0.5).max(20.0);
    }
    if held(VK_OEM_6) {
        s.fov = (s.fov + 0.5).min(140.0);
    }
    if held(K_Z) {
        s.roll -= look;
    }
    if held(K_C) {
        s.roll += look;
    }
    if held(K_X) {
        s.roll = 0.0;
    }
}
