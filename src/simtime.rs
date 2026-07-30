//! Live Blam sim clock.
//!
//! The Blam simulation runs in `HaloSimulation_tag_release.dll` on its own clock. The
//! per-tick delta lives in a `game_time_globals` struct that is reached through the sim
//! DLL's module TLS: `T = *(*(TEB.ThreadLocalStoragePointer[_tls_index]) + 0x98)`.
//!
//! That TLS slot is only populated on the *sim* thread, so we cannot resolve `T` from our
//! game thread directly. But `T` itself is a shared-heap pointer - only the pointer is
//! thread-local. So we walk every thread in the process once, read each thread's TLS
//! slot, and keep the one whose struct matches the signature `tick_length == 1/tick_rate`.
//! Once found, `T` is cached and we read/write it from any thread.
//!
//! Field layout (recovered from the Halo-Script getters):
//!   T+0x06  i16  tick_rate     (== 30)
//!   T+0x08  f32  tick_length   (seconds per tick, ~0.0333) - the live integration delta

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, OpenThread};

const SIM_TLS_INDEX_RVA: usize = 0x00d7_2730; // PE `_tls_index` in the sim DLL
const GTG_TLS_SLOT: usize = 0x98; // TLS block -> game_time_globals
pub const GTG_TICK_RATE: usize = 0x06; // i16 (== 60)
pub const GTG_TICK_LENGTH: usize = 0x08; // f32 seconds/tick (proven live delta; fallback lever)
pub const GTG_GAME_SPEED: usize = 0x10; // f32 uniform time scale (== 1.0 at normal speed)
const THREAD_QUERY_INFORMATION: u32 = 0x0040;
const TEB_TLS_POINTER: usize = 0x58; // TEB.ThreadLocalStoragePointer

static GAME_TIME: AtomicUsize = AtomicUsize::new(0);
static SCAN_TICK: AtomicUsize = AtomicUsize::new(0); // throttle counter for the thread walk
static NORMALIZED: AtomicBool = AtomicBool::new(false); // stock timing restored at boot

type NtQitFn =
    unsafe extern "system" fn(*mut c_void, u32, *mut c_void, u32, *mut u32) -> i32;

unsafe fn nt_qit() -> Option<NtQitFn> {
    let ntdll: Vec<u16> = "ntdll.dll".encode_utf16().chain(core::iter::once(0)).collect();
    let h = GetModuleHandleW(ntdll.as_ptr());
    if h.is_null() {
        return None;
    }
    let p = GetProcAddress(h, b"NtQueryInformationThread\0".as_ptr());
    p.map(|f| core::mem::transmute::<_, NtQitFn>(f))
}

unsafe fn tls_index() -> Option<usize> {
    let sb = crate::mem::sim_base();
    if sb == 0 {
        return None;
    }
    Some(*((sb + SIM_TLS_INDEX_RVA) as *const u32) as usize)
}

/// Signature check (speed-invariant): a plausible userspace pointer with a sane tick rate,
/// a small positive tick_length, an incrementing tick counter, and a positive game_speed.
/// We deliberately do NOT require `tick_length == 1/tick_rate`: while time is being scaled
/// (or the sim is stalled) that identity does not hold, and using it made the resolve fail.
fn looks_like_game_time(t: usize) -> bool {
    if t < 0x1_0000 || t >= 0x0000_8000_0000_0000 {
        return false;
    }
    let mut ok = false;
    crate::seh::guard(|| unsafe {
        let tr = *((t + GTG_TICK_RATE) as *const i16);
        let tl = *((t + GTG_TICK_LENGTH) as *const f32);
        let gt = *((t + 0x0c) as *const i32); // game_tick counter
        let gs = *((t + GTG_GAME_SPEED) as *const f32);
        ok = (24..=240).contains(&tr)
            && tl > 0.0
            && tl < 0.5
            && (100..500_000_000).contains(&gt)
            && gs > 0.0
            && gs < 1000.0;
    });
    ok
}

/// Read one thread's TLS slot for the sim's `game_time_globals`; 0 if absent/mismatched.
unsafe fn read_thread_gt(tid: u32, idx: usize, nt: NtQitFn) -> usize {
    let h = OpenThread(THREAD_QUERY_INFORMATION, 0, tid);
    if h.is_null() {
        return 0;
    }
    let mut tbi = [0u8; 48]; // THREAD_BASIC_INFORMATION; TebBaseAddress at +0x08
    let st = nt(h, 0, tbi.as_mut_ptr() as *mut c_void, 48, core::ptr::null_mut());
    CloseHandle(h);
    if st != 0 {
        return 0;
    }
    let teb = *(tbi.as_ptr().add(8) as *const usize);
    if teb == 0 {
        return 0;
    }
    let mut t = 0usize;
    crate::seh::guard(|| {
        let tls_ptr = *((teb + TEB_TLS_POINTER) as *const usize);
        if tls_ptr == 0 {
            return;
        }
        let tls_block = *((tls_ptr + idx * 8) as *const usize);
        if tls_block == 0 {
            return;
        }
        t = *((tls_block + GTG_TLS_SLOT) as *const usize);
    });
    if looks_like_game_time(t) {
        t
    } else {
        0
    }
}

/// Walk every thread once to locate `game_time_globals`. Logs the outcome.
unsafe fn scan_all_threads() -> usize {
    let idx = match tls_index() {
        Some(i) => i,
        None => {
            crate::rep!("[simtime] sim DLL not loaded");
            return 0;
        }
    };
    let nt = match nt_qit() {
        Some(f) => f,
        None => return 0,
    };
    let pid = GetCurrentProcessId();
    let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
    if snap == INVALID_HANDLE_VALUE {
        return 0;
    }
    let mut te: THREADENTRY32 = core::mem::zeroed();
    te.dwSize = core::mem::size_of::<THREADENTRY32>() as u32;
    let mut found = 0usize;
    let mut scanned = 0u32;
    if Thread32First(snap, &mut te) != 0 {
        loop {
            if te.th32OwnerProcessID == pid {
                scanned += 1;
                let t = read_thread_gt(te.th32ThreadID, idx, nt);
                if t != 0 {
                    found = t;
                    break;
                }
            }
            te.dwSize = core::mem::size_of::<THREADENTRY32>() as u32;
            if Thread32Next(snap, &mut te) == 0 {
                break;
            }
        }
    }
    CloseHandle(snap);
    if found != 0 {
        crate::rep!("[simtime] locked on game_time=0x{:x} ({} threads)", found, scanned);
    }
    found
}

/// One-shot resolve (unthrottled): return the cached pointer if still valid, else do a
/// full scan and cache the result. For user-triggered paths (diagnostic button) only.
pub fn resolve_now() -> usize {
    let cached = GAME_TIME.load(Ordering::Relaxed);
    if cached != 0 && looks_like_game_time(cached) {
        return cached;
    }
    let found = unsafe { scan_all_threads() };
    if found != 0 {
        GAME_TIME.store(found, Ordering::Relaxed);
    }
    found
}

/// Throttled resolve for the command path: returns the cached pointer immediately, or
/// scans at most once every ~30 calls until found. NEVER call this per-frame - the scan
/// walks every thread and would stall the game thread (which manifests as the sim
/// catching up in bursts). The per-frame hold uses [`set_scale`], which never scans.
pub fn ensure() -> usize {
    let t = GAME_TIME.load(Ordering::Relaxed);
    if t != 0 {
        return t;
    }
    if SCAN_TICK.fetch_add(1, Ordering::Relaxed) % 30 != 0 {
        return 0;
    }
    let found = unsafe { scan_all_threads() };
    if found != 0 {
        GAME_TIME.store(found, Ordering::Relaxed);
    }
    found
}

/// Read `(tick_rate, tick_length, game_speed)`, or `None` if the clock is not resolved.
pub fn read() -> Option<(i16, f32, f32)> {
    let t = resolve_now();
    if t == 0 {
        return None;
    }
    let mut out = None;
    crate::seh::guard(|| unsafe {
        let tr = *((t + GTG_TICK_RATE) as *const i16);
        let tl = *((t + GTG_TICK_LENGTH) as *const f32);
        let gs = *((t + GTG_GAME_SPEED) as *const f32);
        out = Some((tr, tl, gs));
    });
    out
}

/// Apply a uniform time scale: write `game_speed` (T+0x10) = `scale`, and pin
/// `tick_length` (T+0x08) back to the stock `1/tick_rate`. Pinning tick_length is
/// deliberate - scaling it directly (an earlier approach) desynced animation from motion
/// AND persisted across restarts (the game saves its timing), which is what caused the
/// out-of-the-box super-speed. Now `game_speed` is the sole scaler and tick_length is
/// always kept stock. `scale < 1` = slow-mo, `> 1` = fast, `== 1` = normal.
/// HOT PATH: cached pointer only, never scans (call [`ensure`] from the command path).
pub fn apply(scale: f32) -> bool {
    let t = GAME_TIME.load(Ordering::Relaxed);
    if t == 0 {
        return false;
    }
    let mut done = false;
    crate::seh::guard(|| unsafe {
        let tr = *((t + GTG_TICK_RATE) as *const i16) as f32;
        if tr >= 1.0 {
            *((t + GTG_TICK_LENGTH) as *mut f32) = 1.0 / tr; // pin stock per-tick delta
            *((t + GTG_GAME_SPEED) as *mut f32) = scale; // uniform time scale
            done = true;
        }
    });
    done
}

/// Boot maintenance (call from the worker loop): resolve the clock (throttled) and, once
/// found, normalize it to stock ONCE - clearing any `tick_length` the game restored from a
/// previous session (which would otherwise run the sim fast/slow out of the box).
pub fn maintain() {
    if NORMALIZED.load(Ordering::Relaxed) {
        return;
    }
    if ensure() == 0 {
        return;
    }
    if apply(1.0) {
        NORMALIZED.store(true, Ordering::Relaxed);
        crate::rep!("[simtime] normalized sim clock to stock (game_speed=1.0, tick_length=1/rate)");
    }
}
