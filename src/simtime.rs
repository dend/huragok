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

/// Read the raw pointer at one thread's TLS `slot`; 0 if the thread has no such block.
unsafe fn read_thread_slot(tid: u32, idx: usize, nt: NtQitFn, slot: usize) -> usize {
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
        t = *((tls_block + slot) as *const usize);
    });
    t
}

/// Walk every thread once, returning the value at TLS `slot` for the first thread whose
/// block passes `valid`. 0 if none.
unsafe fn scan_slot(slot: usize, valid: fn(usize) -> bool) -> usize {
    let idx = match tls_index() {
        Some(i) => i,
        None => return 0,
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
    if Thread32First(snap, &mut te) != 0 {
        loop {
            if te.th32OwnerProcessID == pid {
                let v = read_thread_slot(te.th32ThreadID, idx, nt, slot);
                if v != 0 && valid(v) {
                    found = v;
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
    found
}

/// Locate `game_time_globals` (TLS slot 0x98). Logs on success.
unsafe fn scan_all_threads() -> usize {
    let found = scan_slot(GTG_TLS_SLOT, looks_like_game_time);
    if found != 0 {
        crate::rep!("[simtime] locked on game_time=0x{:x}", found);
    }
    found
}

const GG_TLS_SLOT: usize = 0x60; // TLS block -> sim game-globals block
static GAME_GLOBALS: AtomicUsize = AtomicUsize::new(0);

/// A plausible sim game-globals block: the game/scenario-active gate byte at +0x10 is 1 or 2.
fn looks_like_game_globals(blk: usize) -> bool {
    if blk < 0x1_0000 || blk >= 0x0000_8000_0000_0000 {
        return false;
    }
    let mut ok = false;
    crate::seh::guard(|| unsafe {
        let s = *((blk + 0x10) as *const u8);
        ok = s == 1 || s == 2;
    });
    ok
}

/// Resolve (cached) the sim game-globals block, reached via the sim thread's TLS slot 0x60.
/// Holds difficulty (+0x1E4 sbyte), insertion/checkpoint index (+0x1F0 u16), won (+0x1EBD4).
/// 0 if not currently resolvable. Throttled thread walk when unresolved.
pub fn game_globals() -> usize {
    let cached = GAME_GLOBALS.load(Ordering::Relaxed);
    if cached != 0 && looks_like_game_globals(cached) {
        return cached;
    }
    if SCAN_TICK.fetch_add(1, Ordering::Relaxed) % 30 != 0 {
        return 0;
    }
    let found = unsafe { scan_slot(GG_TLS_SLOT, looks_like_game_globals) };
    GAME_GLOBALS.store(found, Ordering::Relaxed);
    found
}

/// Turn night vision on/off by clearing bit 40 everywhere it is latched (the skull-apply
/// code has no off-branch for NV, so we clear all three additive stores directly):
///   1. global sim skull set  `gameglobals+0x1EBE0`  (render reads live)
///   2. player-effect accumulator  `*(sim+0x2C0C978)` -> `+8` -> `+0x8980/+0x8988` (player 0)
/// All plain writable memory, no TLS beyond the game-globals block. Logs a readback.
pub fn set_night_vision(on: bool) {
    const NV: u64 = 1u64 << 40;
    let gg = game_globals();
    crate::seh::guard(|| unsafe {
        if gg != 0 {
            let p = (gg + 0x1ebe0) as *mut u64;
            let before = *p;
            *p = if on { before | NV } else { before & !NV };
            crate::rep!("[nv] mask 0x{:012x} -> 0x{:012x}", before, *p);
        } else {
            crate::rep!("[nv] game-globals not resolved (toggle again)");
        }
    });
    let sb = crate::mem::sim_base();
    if sb != 0 {
        crate::seh::guard(|| unsafe {
            let base = *((sb + 0x02c0_c978) as *const usize);
            if base > 0x10000 {
                let slot = *((base + 8) as *const usize); // player 0 slot
                if slot > 0x10000 {
                    let a = (slot + 0x8980) as *mut u64;
                    let b = (slot + 0x8988) as *mut u64;
                    let (ba, bb) = (*a, *b);
                    *a = if on { ba | NV } else { ba & !NV };
                    *b = if on { bb | NV } else { bb & !NV };
                    crate::rep!("[nv] accum a 0x{:012x}->0x{:012x} b 0x{:012x}->0x{:012x}", ba, *a, bb, *b);
                }
            }
        });
    }

    // NOTE: a blind sweep of the per-unit trait-latch table (sim+0x174fA40) to clear bit 40
    // corrupts memory - bit 40 (0x010000000000) is a normal bit of a heap pointer, so
    // clearing it across the table truncated live pointers and crashed the sim. Clearing
    // that latch safely needs the player's EXACT unit-table index, not a sweep; deferred.
}

/// Directly set or clear a skull bit in the SIM skull set at `gameglobals+0x1EBE0`. Render
/// effects (e.g. night vision = bit 40) read this qword live every frame, so this toggles
/// them both ways - it is the off-switch the HS `skull_enable false` fails to perform (HS
/// mis-parses `false`, leaving the bit set). Only one transition-driven writer touches this
/// qword, so a direct write is not undone. Returns whether the write landed.
pub fn set_sim_skull(bit: u8, on: bool) -> bool {
    let gg = game_globals();
    if gg == 0 {
        crate::rep!("[skull] sim game-globals not resolved yet; try again");
        return false;
    }
    let mut ok = false;
    crate::seh::guard(|| unsafe {
        let p = (gg + 0x1ebe0) as *mut u64;
        let mask = 1u64 << (bit as u32);
        *p = if on { *p | mask } else { *p & !mask };
        ok = true;
    });
    if ok {
        crate::rep!("[skull] sim skull bit {} {}", bit, if on { "set" } else { "clear" });
    }
    ok
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

/// Return a validated `game_time` pointer, re-resolving (throttled) when the cache is
/// empty or has gone stale - a level reload / respawn can allocate a new struct. The
/// signature check is SEH-guarded, so a freed pointer faults harmlessly and just triggers
/// a rescan. 0 when not currently resolvable. Safe per-frame: it only walks threads while
/// unresolved, at most once every ~30 calls.
fn ptr() -> usize {
    let t = GAME_TIME.load(Ordering::Relaxed);
    if t != 0 && looks_like_game_time(t) {
        return t;
    }
    if SCAN_TICK.fetch_add(1, Ordering::Relaxed) % 30 != 0 {
        return 0;
    }
    let found = unsafe { scan_all_threads() };
    GAME_TIME.store(found, Ordering::Relaxed);
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

static APPLIED_ONCE: AtomicBool = AtomicBool::new(false);

/// Apply a uniform time scale: write `game_speed` (T+0x10) = `scale` and pin `tick_length`
/// (T+0x08) to stock `1/tick_rate`. `game_speed` is the sole scaler; `tick_length` is kept
/// stock because scaling it desyncs animation from motion and persists across restarts.
/// `scale < 1` = slow-mo, `> 1` = fast, `== 1` = normal. Re-validates the clock pointer,
/// so it survives level reloads.
pub fn apply(scale: f32) -> bool {
    let t = ptr();
    if t == 0 {
        return false;
    }
    let mut done = false;
    crate::seh::guard(|| unsafe {
        let tr = *((t + GTG_TICK_RATE) as *const i16) as f32;
        if tr >= 1.0 {
            *((t + GTG_TICK_LENGTH) as *mut f32) = 1.0 / tr;
            *((t + GTG_GAME_SPEED) as *mut f32) = scale;
            done = true;
        }
    });
    if done && !APPLIED_ONCE.swap(true, Ordering::Relaxed) {
        crate::rep!("[simtime] sim clock reached (game_speed live, tick_length pinned stock)");
    }
    done
}

/// Keep `tick_length` at stock `1/tick_rate` WITHOUT touching `game_speed`. Called every
/// frame while the user is not scaling time, so the game's persisted-fast tick_length
/// (which caused super-speed at boot and after a death/checkpoint reload) is corrected
/// continuously. Only writes when it has actually drifted, leaving scripted `game_speed`
/// moments and pause alone. Returns whether the clock was reachable.
pub fn pin_tick_length() -> bool {
    let t = ptr();
    if t == 0 {
        return false;
    }
    let mut ok = false;
    crate::seh::guard(|| unsafe {
        let tr = *((t + GTG_TICK_RATE) as *const i16) as f32;
        if tr >= 1.0 {
            let stock = 1.0 / tr;
            let cur = *((t + GTG_TICK_LENGTH) as *const f32);
            if !cur.is_finite() || (cur - stock).abs() > stock * 0.01 {
                *((t + GTG_TICK_LENGTH) as *mut f32) = stock;
            }
            ok = true;
        }
    });
    ok
}
