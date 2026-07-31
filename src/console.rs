//! Console command input. A reader thread pulls the lines you type into the log console
//! and queues them; the game-thread drain (in the PlayerController detour) runs each one
//! through `ExecuteConsoleCommand`. This is what lets you type e.g.
//! `hs:skull_enable skull_third_person 1` at runtime.

use std::collections::VecDeque;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Storage::FileSystem::ReadFile;
use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};
use windows_sys::Win32::System::Threading::{CreateThread, Sleep};

static QUEUE: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Drain queued input lines. Call on the game thread.
pub fn take_all() -> Vec<String> {
    match QUEUE.lock() {
        Ok(mut q) => q.drain(..).collect(),
        Err(_) => Vec::new(),
    }
}

/// Queue a command line (from the in-game ImGui console input box). The command is echoed
/// once when it actually runs (run_console), so we do not log it again here.
pub fn submit(line: String) {
    if let Ok(mut q) = QUEUE.lock() {
        q.push_back(line);
    }
}

/// Spawn the stdin reader thread (call once, after the console is allocated).
pub fn start() {
    unsafe {
        let h = CreateThread(
            core::ptr::null(),
            0,
            Some(reader),
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
        );
        if !h.is_null() {
            CloseHandle(h);
        }
    }
}

unsafe extern "system" fn reader(_p: *mut core::ffi::c_void) -> u32 {
    let h = GetStdHandle(STD_INPUT_HANDLE);
    let mut buf = [0u8; 1024];
    loop {
        let mut read = 0u32;
        let ok = ReadFile(
            h,
            buf.as_mut_ptr() as *mut _,
            buf.len() as u32,
            &mut read,
            core::ptr::null_mut(),
        );
        if ok == 0 || read == 0 {
            Sleep(150);
            continue;
        }
        let text = String::from_utf8_lossy(&buf[..read as usize]);
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(mut q) = QUEUE.lock() {
                q.push_back(line.to_string());
            }
            crate::rep!("[console] > {}", line);
        }
    }
}
