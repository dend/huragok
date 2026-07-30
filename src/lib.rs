//! Huragok - a gameplay customization engine embedded into Halo: Campaign Evolved.
//!
//! Loaded as a DLL by a `dwmapi` proxy from the game's `mods\` folder. On attach it
//! spins up a worker thread that resolves the module base, waits for the UObject
//! world, and (soon) installs the camera / ImGui / pawn hooks. Named after the
//! Huragok - the living creatures whose whole purpose is to reconfigure technology.

#![allow(non_snake_case, dead_code)]

#[macro_use]
mod log;
mod cmd;
mod hooks;
mod input;
mod mem;
mod offsets;
mod paths;
mod pawn;
mod seh;
mod state;
mod stats;
mod ue;

use core::ffi::c_void;
use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HINSTANCE};
use windows_sys::Win32::System::Threading::{CreateThread, Sleep};

const DLL_PROCESS_ATTACH: u32 = 1;

#[no_mangle]
pub extern "system" fn DllMain(_module: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        unsafe {
            let t = CreateThread(
                core::ptr::null(),
                0,
                Some(run),
                core::ptr::null(),
                0,
                core::ptr::null_mut(),
            );
            if !t.is_null() {
                CloseHandle(t);
            }
        }
    }
    1 // TRUE
}

/// Worker thread: bring the engine online, then hand off to feature hooks.
unsafe extern "system" fn run(_param: *mut c_void) -> u32 {
    log::init_console();
    mem::init();
    log::banner();

    // Wait for a real gameplay PlayerController, then confirm reflection works.
    let pc: *mut u8 = loop {
        if ue::verify() {
            let pc = ue::reflect::find_player_controller();
            if !pc.is_null() {
                rep!("[hook] PlayerController: {}", ue::fname::obj_name(pc));
                break pc;
            }
        }
        Sleep(1000);
    };

    pawn::set_pc(pc);
    hooks::pc::install(pc); // command queue drains here, on the game thread
    rep!("[hook] core online.");

    // Feature loop: keep the camera hook installed, fly the free-cam, poll hotkeys.
    // TODO(port): DrawControls ImGui panel, keyframe paths, HUD overlay.
    loop {
        if !hooks::camera::installed() {
            hooks::camera::install();
        }
        input::poll_freecam();
        input::poll_hotkeys();
        paths::update();
        Sleep(15);
    }
}
