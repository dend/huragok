//! First SIM-THREAD code detour (SWARM9): a trampoline hook on the object-streaming observer-seed
//! builder `0x20ac90`, so the activation flood gets a SECOND seed (the possessed unit's own cluster)
//! in addition to observer 0 (Chief's biped). It runs ON the sim thread (0x20ac90's only caller,
//! `0x20aed0`), so `gs:[0x58]` resolves correctly - the injected work is a plain bounds-checked
//! bit-OR into the resident seed mask arg, NOT a wrong-thread call (which is the crash class we must
//! avoid). See `docs/research/SWARM9_second_seed.md`.
//!
//! Safety design:
//! - The relay is minimal: it calls the original via a trampoline, then ORs ONE precomputed cluster
//!   bit (read from an atomic set on the GAME thread) into the mask. No structure walking, no locks,
//!   no logging on the sim thread.
//! - Patch/unpatch SUSPEND all other threads (the 15-byte patch isn't atomic and the sim thread
//!   calls the target twice/frame), and do NO locking/logging inside the suspend window (a suspended
//!   thread could hold the heap/log lock -> deadlock).
//! - The prologue is self-verified before patching; the trampoline is never freed (a sim thread may
//!   still be executing it after unpatch).

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Memory::{
    VirtualAlloc, VirtualProtect, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, OpenThread, ResumeThread,
    SuspendThread, THREAD_SUSPEND_RESUME,
};

const SEED_BUILDER_RVA: usize = 0x0020_ac90;
const STEAL: usize = 15; // push rbx/rsi/rdi/r14/r15 (8) + sub rsp,0x5b0 (7); clean boundary, no RIP-rel
/// Expected first 8 prologue bytes (push rbx/rsi/rdi/r14/r15) - self-verify before patching.
const EXPECT8: [u8; 8] = [0x40, 0x53, 0x56, 0x57, 0x41, 0x56, 0x41, 0x57];

static TARGET: AtomicUsize = AtomicUsize::new(0); // sim_base + 0x20ac90
static TRAMPOLINE: AtomicUsize = AtomicUsize::new(0); // [STEAL stolen bytes][14-byte abs jmp back]
static INSTALLED: AtomicBool = AtomicBool::new(false);
static HOOK_ON: AtomicBool = AtomicBool::new(true); // inject enabled (A/B toggle)
static HOOK_FIRES: AtomicU64 = AtomicU64::new(0); // relay call count (diag)
const MAX_SEEDS: usize = 32;
// Packed second-seed clusters published on the GAME thread: each entry bit31 = valid, bits8..15 =
// bsp, bits0..7 = cluster (0 = empty slot). These are the clusters of the units AROUND the player;
// the relay ORs each into the seed mask so those clusters stream + animate. The relay only reads
// these atomics - it never walks sim structs on the sim thread.
static SEED_CLUSTERS: [AtomicU32; MAX_SEEDS] = [const { AtomicU32::new(0) }; MAX_SEEDS];
// Throttle the (2048-unit) enumeration: refresh the seed set every Nth update_seed call.
static SEED_TICK: AtomicU32 = AtomicU32::new(0);
const SEED_REFRESH_RADIUS: f32 = 50.0; // world units (~150 m) around the possessed unit
// OFF by default: the aggressive "seed nearby clusters" widen re-rigs dead bodies. Opt-in for A/B.
static SEED_WIDEN: AtomicBool = AtomicBool::new(false);
// FlushInstructionCache fn pointer, resolved ONCE outside the thread-suspend window (GetProcAddress
// takes the loader lock, which a suspended thread could hold -> deadlock).
static FLUSH_FN: AtomicUsize = AtomicUsize::new(0);

/// Resolve `kernel32!FlushInstructionCache` (cached). MUST be called BEFORE suspending threads.
fn resolve_flush() -> usize {
    let c = FLUSH_FN.load(Ordering::Relaxed);
    if c != 0 {
        return c;
    }
    unsafe {
        let k32: Vec<u16> = "kernel32.dll"
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        let h = GetModuleHandleW(k32.as_ptr());
        if h.is_null() {
            return 0;
        }
        match GetProcAddress(h, b"FlushInstructionCache\0".as_ptr()) {
            Some(p) => {
                let v = p as usize;
                FLUSH_FN.store(v, Ordering::Relaxed);
                v
            }
            None => 0,
        }
    }
}

/// Flush the icache via the pre-resolved pointer (no loader lock). `GetCurrentProcess` is a pseudo-
/// handle syscall, safe inside the suspend window. No-op if `flush` is 0.
unsafe fn flush_via(flush: usize, addr: *const c_void, len: usize) {
    if flush != 0 {
        let f: unsafe extern "system" fn(HANDLE, *const c_void, usize) -> i32 =
            core::mem::transmute(flush);
        f(GetCurrentProcess(), addr, len);
    }
}

/// GAME-thread: publish the clusters the sim-thread relay should seed into the activation flood.
/// Currently a NO-OP by default (`SEED_WIDEN` off): widening the active set to animate distant units
/// also re-activates dead bodies in those clusters as rigged corpses (the engine keeps them dormant
/// for that reason), so the aggressive seed is opt-in. With it off we publish nothing -> the hook is
/// installed + proven-safe but inert, and the world is the clean anchor-follow + faction baseline.
pub fn update_seed(unit_handle: u32) {
    if unit_handle == 0xffff_ffff || !SEED_WIDEN.load(Ordering::Relaxed) {
        for s in SEED_CLUSTERS.iter() {
            s.store(0, Ordering::Relaxed);
        }
        return;
    }
    if SEED_TICK.fetch_add(1, Ordering::Relaxed) % 16 != 0 {
        return; // keep the last published set between refreshes
    }
    let clusters =
        crate::simtime::collect_nearby_clusters(unit_handle, SEED_REFRESH_RADIUS, MAX_SEEDS);
    for (i, slot) in SEED_CLUSTERS.iter().enumerate() {
        slot.store(clusters.get(i).copied().unwrap_or(0), Ordering::Relaxed);
    }
}

pub fn set_widen(on: bool) {
    SEED_WIDEN.store(on, Ordering::Relaxed);
}
pub fn widen() -> bool {
    SEED_WIDEN.load(Ordering::Relaxed)
}

pub fn set_enabled(on: bool) {
    HOOK_ON.store(on, Ordering::Relaxed);
}
pub fn enabled() -> bool {
    HOOK_ON.load(Ordering::Relaxed)
}
pub fn fires() -> u64 {
    HOOK_FIRES.load(Ordering::Relaxed)
}
/// Number of clusters currently published for injection (units around the player).
pub fn seed_count() -> usize {
    SEED_CLUSTERS
        .iter()
        .filter(|s| s.load(Ordering::Relaxed) & (1 << 31) != 0)
        .count()
}
pub fn installed() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

/// The detour relay. Runs on the SIM thread with `dst_mask` in rcx and `category` in rdx (matching
/// the original `0x20ac90(mask, category)`). Calls the original via the trampoline, then ORs the
/// precomputed second-seed cluster bit into the mask.
unsafe extern "C" fn relay(dst_mask: usize, category: u64) {
    let tramp = TRAMPOLINE.load(Ordering::Relaxed);
    if tramp != 0 {
        let orig: unsafe extern "C" fn(usize, u64) = core::mem::transmute(tramp);
        orig(dst_mask, category); // engine seeds the real observer(s) into dst_mask
    }
    HOOK_FIRES.fetch_add(1, Ordering::Relaxed);
    if !HOOK_ON.load(Ordering::Relaxed) {
        return;
    }
    // OR each published cluster bit into the seed mask (bounds-checked). One SEH frame around the
    // whole loop - the writes are to the engine's own resident mask arg, bounds-checked, so they
    // never fault; the guard is belt-and-suspenders.
    let _ = crate::seh::guard(|| {
        for s in SEED_CLUSTERS.iter() {
            let packed = s.load(Ordering::Relaxed);
            if packed & (1 << 31) == 0 {
                continue;
            }
            let bsp = ((packed >> 8) & 0xff) as usize;
            let clu = (packed & 0xff) as usize;
            let idx = bsp * 8 + (clu >> 5);
            if idx * 4 + 4 > 0x400 {
                continue; // out of the per-BSP mask (0x400 bytes)
            }
            unsafe {
                *((dst_mask + idx * 4) as *mut u32) |= 1u32 << (clu & 31);
            }
        }
    });
}

/// Build the trampoline once: [STEAL stolen bytes][14-byte abs jmp to target+STEAL]. Returns false
/// if allocation fails. Must be called with the target's prologue already verified.
unsafe fn ensure_trampoline(target: usize) -> bool {
    if TRAMPOLINE.load(Ordering::Relaxed) != 0 {
        return true;
    }
    let tramp = VirtualAlloc(
        core::ptr::null(),
        64,
        MEM_COMMIT | MEM_RESERVE,
        PAGE_EXECUTE_READWRITE,
    ) as usize;
    if tramp == 0 {
        return false;
    }
    core::ptr::copy_nonoverlapping(target as *const u8, tramp as *mut u8, STEAL);
    // abs jmp back: FF 25 00000000 <qword target+STEAL>
    let jb = (tramp + STEAL) as *mut u8;
    *jb = 0xff;
    *jb.add(1) = 0x25;
    *(jb.add(2) as *mut u32) = 0;
    *(jb.add(6) as *mut u64) = (target + STEAL) as u64;
    TRAMPOLINE.store(tramp, Ordering::Relaxed);
    true
}

/// Install the hook on the possessed session's behalf. Idempotent. Suspends other threads for the
/// 15-byte patch. Safe to call on the game thread.
pub fn install() {
    if INSTALLED.load(Ordering::Relaxed) {
        return;
    }
    let sb = crate::mem::sim_base();
    if sb == 0 {
        return;
    }
    let target = sb + SEED_BUILDER_RVA;
    let mut ok = false;
    let _ = crate::seh::guard(|| unsafe {
        ok = core::slice::from_raw_parts(target as *const u8, 8) == EXPECT8;
    });
    if !ok {
        crate::rep!("[seedhook] prologue mismatch @0x{target:x} - refusing to patch");
        return;
    }
    if unsafe { !ensure_trampoline(target) } {
        crate::rep!("[seedhook] VirtualAlloc(trampoline) failed");
        return;
    }
    TARGET.store(target, Ordering::Relaxed);
    // Build the patch: FF 25 00000000 <qword relay> + NOP pad to STEAL.
    let mut patch = [0x90u8; STEAL];
    patch[0] = 0xff;
    patch[1] = 0x25;
    patch[2..6].copy_from_slice(&0u32.to_le_bytes());
    patch[6..14].copy_from_slice(&(relay as *const () as usize as u64).to_le_bytes());
    let flush = resolve_flush(); // resolve BEFORE suspending (loader lock)
    let suspended = suspend_others();
    // NO locking / logging / heap alloc / name resolution in the suspend window (a suspended thread
    // may hold the heap/loader/log lock).
    let _ = crate::seh::guard(|| unsafe {
        let mut old = 0u32;
        if VirtualProtect(target as *mut c_void, STEAL, PAGE_EXECUTE_READWRITE, &mut old) != 0 {
            core::ptr::copy_nonoverlapping(patch.as_ptr(), target as *mut u8, STEAL);
            let mut tmp = 0u32;
            VirtualProtect(target as *mut c_void, STEAL, old, &mut tmp);
            flush_via(flush, target as *const c_void, STEAL);
        }
    });
    resume_all(&suspended);
    INSTALLED.store(true, Ordering::Relaxed);
    crate::rep!(
        "[seedhook] installed @0x{target:x} tramp=0x{:x}",
        TRAMPOLINE.load(Ordering::Relaxed)
    );
}

/// Remove the hook (restore the stolen bytes). Keeps the trampoline allocated (a sim thread may still
/// be executing it). Idempotent.
pub fn uninstall() {
    if !INSTALLED.swap(false, Ordering::Relaxed) {
        return;
    }
    for s in SEED_CLUSTERS.iter() {
        s.store(0, Ordering::Relaxed);
    }
    let target = TARGET.load(Ordering::Relaxed);
    let tramp = TRAMPOLINE.load(Ordering::Relaxed);
    if target == 0 || tramp == 0 {
        return;
    }
    let flush = resolve_flush(); // resolve BEFORE suspending (loader lock)
    let suspended = suspend_others();
    let _ = crate::seh::guard(|| unsafe {
        let mut old = 0u32;
        if VirtualProtect(target as *mut c_void, STEAL, PAGE_EXECUTE_READWRITE, &mut old) != 0 {
            // Restore the original bytes from the trampoline's first STEAL bytes.
            core::ptr::copy_nonoverlapping(tramp as *const u8, target as *mut u8, STEAL);
            let mut tmp = 0u32;
            VirtualProtect(target as *mut c_void, STEAL, old, &mut tmp);
            flush_via(flush, target as *const c_void, STEAL);
        }
    });
    resume_all(&suspended);
    crate::rep!("[seedhook] uninstalled @0x{target:x}");
}

/// Suspend every other thread in this process (so the non-atomic code patch can't be executed
/// mid-write). Two passes: enumerate thread IDs first (heap alloc OK, nothing suspended yet), then
/// OpenThread+SuspendThread into a PRE-SIZED vec - so no heap realloc happens while a thread (which
/// might hold the heap lock) is suspended. Returns the suspended handles for `resume_all`.
fn suspend_others() -> Vec<HANDLE> {
    let mut ids: Vec<u32> = Vec::new();
    unsafe {
        let pid = GetCurrentProcessId();
        let me = GetCurrentThreadId();
        // Pass 1: enumerate thread IDs (no suspension -> heap alloc is safe here).
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Vec::new();
        }
        let mut te: THREADENTRY32 = core::mem::zeroed();
        te.dwSize = core::mem::size_of::<THREADENTRY32>() as u32;
        if Thread32First(snap, &mut te) != 0 {
            loop {
                if te.th32OwnerProcessID == pid && te.th32ThreadID != me {
                    ids.push(te.th32ThreadID);
                }
                te.dwSize = core::mem::size_of::<THREADENTRY32>() as u32;
                if Thread32Next(snap, &mut te) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        // Pass 2: suspend into a pre-sized vec (no realloc while threads are suspended).
        let mut handles: Vec<HANDLE> = Vec::with_capacity(ids.len());
        for &tid in &ids {
            let h = OpenThread(THREAD_SUSPEND_RESUME, 0, tid);
            if !h.is_null() {
                if SuspendThread(h) as i32 != -1 {
                    handles.push(h);
                } else {
                    CloseHandle(h);
                }
            }
        }
        handles
    }
}

fn resume_all(handles: &[HANDLE]) {
    unsafe {
        for &h in handles {
            ResumeThread(h);
            CloseHandle(h);
        }
    }
}
