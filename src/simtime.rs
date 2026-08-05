//! Live Blam sim clock.
//!
//! The Blam simulation runs in its own module on its own clock. The
//! per-tick delta lives in a `game_time_globals` struct that is reached through the sim
//! DLL's module TLS: `T = *(*(TEB.ThreadLocalStoragePointer[_tls_index]) + 0x98)`.
//!
//! That TLS slot is only populated on the *sim* thread, so we cannot resolve `T` from our
//! game thread directly. But `T` itself is a shared-heap pointer - only the pointer is
//! thread-local. So we walk every thread in the process once, read each thread's TLS
//! slot, and keep the one whose struct matches the signature `tick_length == 1/tick_rate`.
//! Once found, `T` is cached and we read/write it from any thread.
//!
//! Field layout (recovered from the sim-script getters):
//!   T+0x06  i16  tick_rate     (== 30)
//!   T+0x08  f32  tick_length   (seconds per tick, ~0.0333) - the live integration delta

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Memory::{VirtualProtect, PAGE_EXECUTE_READWRITE, PAGE_READWRITE};
use windows_sys::Win32::System::Threading::GetCurrentProcess;
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

// ---- Local player's Blam unit object-body pointer (for direct sim cheats) ----
// Chain (offsets into the sim thread's TLS block, then the object table), recovered by
// static RE and cross-checked against `unit_get_health`:
//   pcg          = *(tls + 0x118)                    ; player-control globals
//   player_h     = *(u32*)(pcg + 0xB8)               ; local player 0 handle (0xFFFFFFFF = none)
//   players_base = *(*(tls + 0x30) + 0x50)
//   unit_h       = *(u32*)(players_base + (player_h & 0xFFFF)*0x4B0 + 0x28)  ; (0xFFFFFFFF = none)
//   object_table = *(*(tls + 0x20) + 0x50)
//   U            = *(object_table + (unit_h & 0xFFFF)*0x18 + 0x10)           ; the object body
const PCG_TLS_SLOT: usize = 0x118;
const PLAYERS_TLS_SLOT: usize = 0x30;
const OBJTABLE_TLS_SLOT: usize = 0x20;
const PCG_LOCAL0_HANDLE: usize = 0xb8;
const PLAYERS_TABLE_OFF: usize = 0x50;
const S_PLAYER_STRIDE: usize = 0x4b0;
const S_PLAYER_UNIT_HANDLE: usize = 0x28;
const OBJTABLE_BASE_OFF: usize = 0x50;
const OBJ_RECORD_STRIDE: usize = 0x18;
const OBJ_RECORD_BODY: usize = 0x10;

static SIM_TLS_BLOCK: AtomicUsize = AtomicUsize::new(0);
static SCAN_TICK_TLS: AtomicUsize = AtomicUsize::new(0);

/// Read the sim DLL's TLS block pointer for one thread (0 if it has none).
unsafe fn read_thread_tls_block(tid: u32, idx: usize, nt: NtQitFn) -> usize {
    let h = OpenThread(THREAD_QUERY_INFORMATION, 0, tid);
    if h.is_null() {
        return 0;
    }
    let mut tbi = [0u8; 48];
    let st = nt(h, 0, tbi.as_mut_ptr() as *mut c_void, 48, core::ptr::null_mut());
    CloseHandle(h);
    if st != 0 {
        return 0;
    }
    let teb = *(tbi.as_ptr().add(8) as *const usize);
    if teb == 0 {
        return 0;
    }
    let mut block = 0usize;
    crate::seh::guard(|| {
        let tls_ptr = *((teb + TEB_TLS_POINTER) as *const usize);
        if tls_ptr != 0 {
            block = *((tls_ptr + idx * 8) as *const usize);
        }
    });
    block
}

/// True if this TLS block belongs to the sim thread (its game-globals slot resolves).
fn is_sim_tls_block(block: usize) -> bool {
    if block < 0x1_0000 || block >= 0x0000_8000_0000_0000 {
        return false;
    }
    let mut gg = 0usize;
    let ok = crate::seh::guard(|| unsafe {
        gg = *((block + GG_TLS_SLOT) as *const usize);
    });
    ok && looks_like_game_globals(gg)
}

/// Resolve (cached) the sim thread's TLS block so we can read the object/player slots off it.
/// Throttled thread walk while unresolved; the block persists for the thread's lifetime.
fn sim_tls_block() -> usize {
    let cached = SIM_TLS_BLOCK.load(Ordering::Relaxed);
    if cached != 0 && is_sim_tls_block(cached) {
        return cached;
    }
    if SCAN_TICK_TLS.fetch_add(1, Ordering::Relaxed) % 30 != 0 {
        return 0;
    }
    let found = unsafe {
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
        let mut block = 0usize;
        if Thread32First(snap, &mut te) != 0 {
            loop {
                if te.th32OwnerProcessID == pid {
                    let b = read_thread_tls_block(te.th32ThreadID, idx, nt);
                    if b != 0 && is_sim_tls_block(b) {
                        block = b;
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
        block
    };
    SIM_TLS_BLOCK.store(found, Ordering::Relaxed);
    found
}

/// Resolve the local player's Blam unit object-body pointer `U`, or 0 if unavailable.
/// Cheap once the TLS block is cached; every deref is SEH-guarded so a stale handle cannot
/// crash us. The caller must still validate `U` (vitality-array sanity) before writing.
pub fn player_unit() -> usize {
    let tls = sim_tls_block();
    if tls == 0 {
        return 0;
    }
    let mut u = 0usize;
    crate::seh::guard(|| unsafe {
        let pcg = *((tls + PCG_TLS_SLOT) as *const usize);
        if pcg == 0 {
            return;
        }
        let player_h = *((pcg + PCG_LOCAL0_HANDLE) as *const u32);
        if player_h == 0xffff_ffff {
            return;
        }
        let player_idx = (player_h & 0xffff) as usize;

        let players_glob = *((tls + PLAYERS_TLS_SLOT) as *const usize);
        if players_glob == 0 {
            return;
        }
        let players_base = *((players_glob + PLAYERS_TABLE_OFF) as *const usize);
        if players_base == 0 {
            return;
        }
        let unit_h = *((players_base + player_idx * S_PLAYER_STRIDE + S_PLAYER_UNIT_HANDLE)
            as *const u32);
        if unit_h == 0xffff_ffff {
            return;
        }
        let unit_idx = (unit_h & 0xffff) as usize;

        let obj_glob = *((tls + OBJTABLE_TLS_SLOT) as *const usize);
        if obj_glob == 0 {
            return;
        }
        let table = *((obj_glob + OBJTABLE_BASE_OFF) as *const usize);
        if table == 0 {
            return;
        }
        u = *((table + unit_idx * OBJ_RECORD_STRIDE + OBJ_RECORD_BODY) as *const usize);
    });
    u
}

/// Resolve an object body from a Blam datum handle via the sim object table (0 if bad).
/// Same tail as [`player_unit`], but for an arbitrary unit handle (used by possession).
pub fn object_body(handle: u32) -> usize {
    if handle == 0xffff_ffff {
        return 0;
    }
    let tls = sim_tls_block();
    if tls == 0 {
        return 0;
    }
    let mut u = 0usize;
    crate::seh::guard(|| unsafe {
        let obj_glob = *((tls + OBJTABLE_TLS_SLOT) as *const usize);
        if obj_glob == 0 {
            return;
        }
        let table = *((obj_glob + OBJTABLE_BASE_OFF) as *const usize);
        if table == 0 {
            return;
        }
        let idx = (handle & 0xffff) as usize;
        u = *((table + idx * OBJ_RECORD_STRIDE + OBJ_RECORD_BODY) as *const usize);
    });
    u
}

/// Collect up to `max` DISTINCT streaming clusters (packed `bit31 | bsp<<8 | clu`) of unit bodies
/// within `radius` world-units of the center unit (SWARM9 seed hook). Seeding these clusters makes
/// the units AROUND the player stream + animate - covering the PVS-disconnected clusters the on-unit
/// observer can't reach (the hill/forward skaters). The possessed unit's own cluster is included
/// (distance 0). Returns the packed set (game-thread read; the sim-thread relay just ORs the bits).
pub fn collect_nearby_clusters(center_handle: u32, radius: f32, max: usize) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::with_capacity(max);
    let cb = object_body(center_handle);
    if cb == 0 {
        return out;
    }
    let table = object_table();
    if table == 0 {
        return out;
    }
    let r2 = radius * radius;
    let _ = crate::seh::guard(|| unsafe {
        let cp = (cb + 0x44) as *const f32;
        let (cx, cy, cz) = (*cp, *cp.add(1), *cp.add(2));
        for i in 0..2048usize {
            if out.len() >= max {
                break;
            }
            let rec = table + i * OBJ_RECORD_STRIDE;
            let body = *((rec + OBJ_RECORD_BODY) as *const usize);
            if body == 0 || !crate::simunit::valid_unit(body) {
                continue;
            }
            let p = (body + 0x44) as *const f32;
            let (dx, dy, dz) = (*p - cx, *p.add(1) - cy, *p.add(2) - cz);
            if dx * dx + dy * dy + dz * dz > r2 {
                continue;
            }
            let bsp = *((body + 0x8) as *const i8);
            if bsp < 0 {
                continue;
            }
            let clu = *((body + 0x9) as *const u8);
            let packed = (1u32 << 31) | ((bsp as u32 & 0xff) << 8) | (clu as u32);
            if !out.contains(&packed) {
                out.push(packed);
            }
        }
    });
    out
}

/// (diag, SWARM9) A unit's streaming cluster + active flags: (bsp `i8[body+0x8]`, cluster
/// `u8[body+0x9]`, `u32[body+0x4]`). The seed/active-mask membership bit is derived from bsp/cluster
/// (`mask[bsp*8 + (clu>>5)] & (1<<(clu&31))`); `body+0x4` bit `0x2` = object-active. None if unresolved.
pub fn object_cluster(handle: u32) -> Option<(i8, u8, u32)> {
    let body = object_body(handle);
    if body == 0 {
        return None;
    }
    let mut out = None;
    let _ = crate::seh::guard(|| unsafe {
        out = Some((
            *((body + 0x8) as *const i8),
            *((body + 0x9) as *const u8),
            *((body + 0x4) as *const u32),
        ));
    });
    out
}

/// (diag, SWARM9) Size of the ACTIVE cluster set = popcount of mask C (`game_globals+0x1f3f4`, 0x400
/// bytes) — the streaming zone breadth. Also returns word0 of seed masks A (`+0x1ebf4`) and B
/// (`+0x1eff4`) for a quick sanity sample. This is the number that should JUMP when the second seed
/// is injected. None if the sim game-globals block isn't resolved.
pub fn active_cluster_popcount() -> Option<(u32, u32, u32)> {
    let gg = game_globals();
    if gg == 0 {
        return None;
    }
    let mut out = None;
    let _ = crate::seh::guard(|| unsafe {
        let c = (gg + 0x1f3f4) as *const u32;
        let mut pop = 0u32;
        for i in 0..0x100usize {
            pop += (*c.add(i)).count_ones();
        }
        out = Some((pop, *((gg + 0x1ebf4) as *const u32), *((gg + 0x1eff4) as *const u32)));
    });
    out
}

/// (diag, SWARM9) The 4 co-op player-observer handles (`PCG+0xb8 + i*4`). Single-player populates
/// only slot 0 (the rest read 0xFFFFFFFF) — confirms the single-seed topology the fix widens.
pub fn observer_handles() -> [u32; 4] {
    let mut out = [0xffff_ffffu32; 4];
    let tls = sim_tls_block();
    if tls == 0 {
        return out;
    }
    let _ = crate::seh::guard(|| unsafe {
        let pcg = *((tls + PCG_TLS_SLOT) as *const usize);
        if pcg != 0 {
            for i in 0..4usize {
                out[i] = *((pcg + PCG_LOCAL0_HANDLE + i * 4) as *const u32);
            }
        }
    });
    out
}

/// Move the streaming/activation ANCHOR biped (`anchor_h` = Chief's real sim biped, SAVED_PLAYER_HANDLE)
/// to follow the possessed unit (`follow_h`) so the engine streams + animates the AI around the
/// possessed body itself (SWARM8e). Possession leaves Chief's sim biped frozen at the possession spot,
/// and it is the anchor the streaming system reads - so the AI around our new position never stream in
/// ("skating"), which is exactly why physically walking Chief over fixes it. This is a PLAIN
/// resolved-object memory store (body+0x44 world position) - the same safety class as the mod's
/// body+0x1BC/0x1AC writes, NOT a gs:[]-resolving sim call - so it cannot trigger the wrong-thread
/// crash that force-activation did. Chief's biped is a live, sim-ticked object, so the sim thread
/// re-derives its cluster from the jumped position on its own. `up` lifts him above the possessed
/// unit (sim world units) so his capsule doesn't shove it. `lead` places him that many world units
/// AHEAD along the possessed unit's horizontal velocity (SWARM8f): activation is cluster/BSP-PVS based
/// (no scalar radius exists), so leading the anchor pulls the active-cluster set toward where we're
/// heading, streaming the AI ahead in before we reach them. Lead collapses to 0 when ~stationary, so
/// at a destination it's exactly the proven anchor-on-unit behavior. Returns the distance jumped.
pub fn anchor_biped_to(anchor_h: u32, follow_h: u32, lead: f32, up: f32) -> Option<f32> {
    let ab = object_body(anchor_h);
    let fb = object_body(follow_h);
    if ab == 0 || fb == 0 {
        return None;
    }
    let mut moved = None;
    let _ = crate::seh::guard(|| unsafe {
        let fp = (fb + 0x44) as *const f32;
        // Lead along horizontal velocity (body+0x68). Zero lead when nearly still keeps the anchor on
        // the unit (the proven state); direction from velocity avoids any yaw-frame convention guess.
        let v = (fb + 0x68) as *const f32;
        let (vx, vy) = (*v, *v.add(1));
        let vlen = (vx * vx + vy * vy).sqrt();
        let (lx, ly) = if vlen > 0.1 {
            (lead * vx / vlen, lead * vy / vlen)
        } else {
            (0.0, 0.0)
        };
        let (tx, ty, tz) = (*fp + lx, *fp.add(1) + ly, *fp.add(2) + up);
        let ap = (ab + 0x44) as *mut f32;
        let d = ((*ap - tx).powi(2) + (*ap.add(1) - ty).powi(2) + (*ap.add(2) - tz).powi(2)).sqrt();
        *ap = tx;
        *ap.add(1) = ty;
        *ap.add(2) = tz;
        moved = Some(d);
    });
    moved
}

/// Address of the local player's unit-handle slot (0 if the chain can't be resolved).
/// Transient sim memory - read/write it immediately under SEH.
fn player_unit_slot() -> usize {
    let tls = sim_tls_block();
    if tls == 0 {
        return 0;
    }
    let mut slot = 0usize;
    crate::seh::guard(|| unsafe {
        let pcg = *((tls + PCG_TLS_SLOT) as *const usize);
        if pcg == 0 {
            return;
        }
        let player_h = *((pcg + PCG_LOCAL0_HANDLE) as *const u32);
        if player_h == 0xffff_ffff {
            return;
        }
        let player_idx = (player_h & 0xffff) as usize;
        let players_glob = *((tls + PLAYERS_TLS_SLOT) as *const usize);
        if players_glob == 0 {
            return;
        }
        let players_base = *((players_glob + PLAYERS_TABLE_OFF) as *const usize);
        if players_base == 0 {
            return;
        }
        slot = players_base + player_idx * S_PLAYER_STRIDE + S_PLAYER_UNIT_HANDLE;
    });
    slot
}

/// Address of the local player's OBSERVER team byte `player_struct+0xAC` (SWARM6_faction: the radar/
/// nav friend-foe classifiers read this as the observer team; target team is `unit+0x1BA`). Since
/// `player_unit_slot()` = `player_struct + 0x28`, this is `player_unit_slot() - 0x28 + 0xAC`. 0 if
/// unresolved. Transient sim memory - read/write immediately under SEH.
fn player_team_byte_slot() -> usize {
    let s = player_unit_slot();
    if s == 0 {
        return 0;
    }
    s - S_PLAYER_UNIT_HANDLE + 0xac
}

/// Read the local player's observer team byte (`player_struct+0xAC`).
pub fn player_team_byte() -> Option<i8> {
    let s = player_team_byte_slot();
    if s == 0 {
        return None;
    }
    let mut v = None;
    let _ = crate::seh::guard(|| unsafe { v = Some(*(s as *const i8)) });
    v
}

/// Write the observer team byte; returns the previous value (for restore). Writing this = making the
/// local player perceive as `team`, which flips BOTH friend-foe directions (radar + who targets you).
pub fn set_player_team_byte(team: i8) -> Option<i8> {
    let s = player_team_byte_slot();
    if s == 0 {
        return None;
    }
    let mut old = None;
    let _ = crate::seh::guard(|| unsafe {
        let p = s as *mut i8;
        old = Some(*p);
        *p = team;
    });
    old
}

/// Restore the observer team byte to `team`.
pub fn restore_player_team_byte(team: i8) {
    let s = player_team_byte_slot();
    if s != 0 {
        let _ = crate::seh::guard(|| unsafe { *(s as *mut i8) = team });
    }
}

/// (kick experiment) Cheap "scenario still active" probe: `(gate = game_globals+0x10, won =
/// game_globals+0x1EBD4)`. `game_globals()` returns 0 once the scenario tears down, so a kick-to-menu
/// shows up here as `None`. Pair with `player_unit() != 0` to separate a menu-kick from a death-fail.
pub fn mission_state() -> Option<(u8, u8)> {
    let blk = game_globals();
    if blk == 0 {
        return None;
    }
    let mut out = None;
    let _ = crate::seh::guard(|| unsafe {
        out = Some((*((blk + 0x10) as *const u8), *((blk + 0x1ebd4) as *const u8)));
    });
    out
}

// Control-input element: the SECOND player->unit binding. ctl_apply (@0x278660) reads the
// player's look + trigger from this element and applies aim (body+0x204..) and weapon-fire
// flags to the unit named at element+0x80 - a binding distinct from the movement bind slot.
const CTL_BUF_TLS_SLOT: usize = 0xB8; // simTLS+0xB8 -> control-element buffer
const PCG_CTLSLOT_BASE: usize = 0x74; // pcg + 0x74 + player_idx*4 -> control slot index
const CTL_ELEM_STRIDE: usize = 0x198;
const CTL_ELEM_UNIT: usize = 0x80; // u32 controlled-unit handle (aim/fire target)

/// Address of the local player's control-element unit slot (element+0x80), 0 if unresolved.
fn control_unit_slot() -> usize {
    let tls = sim_tls_block();
    if tls == 0 {
        return 0;
    }
    let mut slot = 0usize;
    let _ = crate::seh::guard(|| unsafe {
        let pcg = *((tls + PCG_TLS_SLOT) as *const usize);
        if pcg == 0 {
            return;
        }
        let player_h = *((pcg + PCG_LOCAL0_HANDLE) as *const u32);
        if player_h == 0xffff_ffff {
            return;
        }
        let pidx = (player_h & 0xffff) as usize;
        let ctl_slot = *((pcg + PCG_CTLSLOT_BASE + pidx * 4) as *const i32);
        if ctl_slot < 0 {
            return;
        }
        let ctlbuf = *((tls + CTL_BUF_TLS_SLOT) as *const usize);
        if ctlbuf == 0 {
            return;
        }
        slot = ctlbuf + (ctl_slot as usize) * CTL_ELEM_STRIDE + CTL_ELEM_UNIT;
    });
    slot
}

/// Point the aim/fire control target at `new_h`, returning the previous value (None if the
/// chain is unresolved). Pairs with `set_player_unit_handle` to fully redirect a player.
pub fn set_control_unit_handle(new_h: u32) -> Option<u32> {
    let s = control_unit_slot();
    if s == 0 {
        return None;
    }
    let mut old = None;
    let _ = crate::seh::guard(|| unsafe {
        let p = s as *mut u32;
        old = Some(*p);
        *p = new_h;
    });
    old
}

/// Re-assert the aim/fire control target (cheap per-frame write; no read-back).
pub fn assert_control_unit_handle(h: u32) {
    let s = control_unit_slot();
    if s != 0 {
        let _ = crate::seh::guard(|| unsafe { *(s as *mut u32) = h });
    }
}

/// Restore third-person LOCOMOTION animation on a +0x1BC-owned unit (SWARM7_self_legs). +0x1BC puts
/// the unit in first-person OWNER anim mode - body+0xDC bit 0x100 (FP-anim flag) set + anim-state
/// body+0xE4 = 0x8000 ("none") - so its legs freeze. Clear the FP bit (moves it onto the native TP
/// gait code path) and reset the anim-state to 0xFFFF ("re-select") so the velocity-driven gait
/// selector (0x6BF640) repopulates it. Both writes are CONDITIONAL/idempotent - once a real state
/// installs we stop touching it. Call each tick AFTER the +0x1BC re-assert; the ownership stamp is
/// edge-triggered, so a steady +0x1BC won't re-set the flag.
pub fn restore_tp_animation(unit_handle: u32) {
    let b = object_body(unit_handle);
    if b == 0 {
        return;
    }
    let _ = crate::seh::guard(|| unsafe {
        let dc = (b + 0xdc) as *mut u32;
        if *dc & 0x100 != 0 {
            *dc &= !0x100;
        }
        let e4 = (b + 0xe4) as *mut u16;
        if *e4 == 0x8000 {
            *e4 = 0xffff;
        }
    });
}

/// (diag) the possessed unit's anim mode/state fields: (body+0xDC flags, body+0xE4 state, body+0xCC).
pub fn anim_diag(unit_handle: u32) -> Option<(u32, u16, u32)> {
    let b = object_body(unit_handle);
    if b == 0 {
        return None;
    }
    let mut out = None;
    let _ = crate::seh::guard(|| unsafe {
        out = Some((
            *((b + 0xdc) as *const u32),
            *((b + 0xe4) as *const u16),
            *((b + 0xcc) as *const u32),
        ));
    });
    out
}

/// The unit's team WORD (unit body+0x1BA, u16 = team index +0x1BA and allegiance +0x1BB). Read as
/// a word so we carry both bytes. Read live rather than hard-coding a faction.
pub fn unit_team(unit_handle: u32) -> Option<u16> {
    let b = object_body(unit_handle);
    if b == 0 {
        return None;
    }
    let mut t = None;
    let _ = crate::seh::guard(|| unsafe { t = Some(*((b + 0x1ba) as *const u16)) });
    t
}

// The +0x1BC team-sync (0x182cb0) stamps word[player+0xAC] -> unit+0x1BA EVERY FRAME while a unit
// is controlled, forcing the possessed unit's team back to the player's UNSC team - so holding it
// Covenant per-tick fought the sync and flickered the radar/weapon/vignette. This one-byte patch at
// 0x182D31 turns the `je` guarding the team-copy into a `jmp`, skipping ONLY the
// `movzx eax,word[+0xAC]; mov [+0x1BA],ax` pair (the sole per-frame +0x1BA writer); the rest of the
// sync (control-element registration for aim/fire) still runs. player+0xAC is never touched -> no
// mission-end kick. Self-verifying (only flips the expected byte).
const TEAM_SYNC_PATCH_RVA: usize = 0x0018_2d31;

/// FlushInstructionCache resolved dynamically from kernel32 (its windows_sys module isn't enabled).
pub(crate) unsafe fn flush_icache(addr: *const c_void, len: usize) {
    let k32: Vec<u16> = "kernel32.dll".encode_utf16().chain(core::iter::once(0)).collect();
    let h = GetModuleHandleW(k32.as_ptr());
    if h.is_null() {
        return;
    }
    if let Some(p) = GetProcAddress(h, b"FlushInstructionCache\0".as_ptr()) {
        let f: unsafe extern "system" fn(*mut c_void, *const c_void, usize) -> i32 =
            core::mem::transmute(p);
        f(GetCurrentProcess(), addr, len);
    }
}

/// Disable (`disable=true`, je 0x74 -> jmp 0xEB) or restore the team-sync's team-copy. Returns
/// whether the byte was flipped. After disabling, one `set_unit_team(h, covenant)` holds forever.
pub fn patch_team_sync(disable: bool) -> bool {
    let sb = crate::mem::sim_base();
    if sb == 0 {
        return false;
    }
    let addr = (sb + TEAM_SYNC_PATCH_RVA) as *mut u8;
    let (from, to) = if disable { (0x74u8, 0xebu8) } else { (0xebu8, 0x74u8) };
    let mut ok = false;
    let _ = crate::seh::guard(|| unsafe {
        if *addr != from {
            return; // unexpected/already-set byte - refuse to touch code
        }
        let mut old = 0u32;
        if VirtualProtect(addr as *mut c_void, 1, PAGE_EXECUTE_READWRITE, &mut old) != 0 {
            *addr = to;
            let mut tmp = 0u32;
            VirtualProtect(addr as *mut c_void, 1, old, &mut tmp);
            flush_icache(addr as *const c_void, 1);
            ok = true;
        }
    });
    if ok {
        crate::rep!(
            "[faction] team-sync {} @0x{:x}",
            if disable { "disabled (je->jmp)" } else { "restored" },
            addr as usize
        );
    }
    ok
}

/// Force the possessed UNIT's team word (unit body+0x1BA) directly. The radar/AI read the observer
/// (player-controlled) unit's team, so holding this at the unit's own Covenant team makes the sect
/// read as allies. Setting +0x1BC engages a per-frame sync that copies the PLAYER team (+0xAC, UNSC)
/// into +0x1BA, so we re-assert this each tick to keep the unit Covenant. We do NOT change the
/// player's own team (+0xAC) - doing so ends the campaign mission (player becomes hostile-team).
pub fn set_unit_team(unit_handle: u32, team: u16) {
    let b = object_body(unit_handle);
    if b == 0 {
        return;
    }
    let _ = crate::seh::guard(|| unsafe { *((b + 0x1ba) as *mut u16) = team });
}

/// Read/write the unit's AI datum handle (body+0x1AC). Setting it to 0xFFFFFFFF (-1, the spawn
/// "no AI" sentinel) DETACHES the AI actor from the unit, removing the AI facing-controller from
/// the per-unit facing-function stack. This is the jitter fix: while +0x1BC makes the player-aim
/// controller active, a merely-braindead (not detached) AI keeps its own facing-function in the
/// stack, and the per-frame selector alternates the two -> the facing flips frame to frame. With
/// the AI detached the player-aim controller owns facing every frame. Returns the previous value.
pub fn set_unit_ai_datum(unit_handle: u32, val: u32) -> Option<u32> {
    let b = object_body(unit_handle);
    if b == 0 {
        return None;
    }
    let mut old = None;
    let _ = crate::seh::guard(|| unsafe {
        let p = (b + BODY_AI_REF) as *mut u32;
        old = Some(*p);
        *p = val;
    });
    old
}

/// The player's SMOOTH look angles `(yaw, pitch)` in radians, read from the control-element input
/// accumulators (element+0x94 yaw, +0x98 pitch). Native mouse-look is integrated into these once
/// per frame BEFORE the biped facing motor, so they are stable - unlike body+0x50/+0x204, which the
/// motor whips. The camera reads this for a smooth third-person view with real pitch. None if the
/// element is unresolved or the values look wrong.
pub fn player_look_angles() -> Option<(f32, f32)> {
    let slot = control_unit_slot();
    if slot == 0 {
        return None;
    }
    let base = slot - CTL_ELEM_UNIT;
    let mut out = None;
    let _ = crate::seh::guard(|| unsafe {
        let yaw = *((base + 0x94) as *const f32);
        let pitch = *((base + 0x98) as *const f32);
        out = Some((yaw, pitch));
    });
    match out {
        Some((y, p)) if y.is_finite() && p.is_finite() && y.abs() < 7.0 && p.abs() < 2.0 => {
            Some((y, p))
        }
        _ => None,
    }
}

// The control element's look direction the sim actually uses. ctl_apply (0x278660) maps it onto the
// bound unit's aim/facing (body+0x204/+0x1D4), and native MOVEMENT is relative to that facing - so
// this, NOT UE ControlRotation, is what steers WASD-forward. Candidate offsets from RE (verify via
// dump_control_element): element+0x94/98/9C looks like a forward vec3.
const CTL_ELEM_LOOK: usize = 0x94;

/// Write the control-element look vector (element+0x94/98/9C) = `fwd` (a sim-space forward vec3).
/// This is what native mouse-look would write; supplying it from our camera look makes native
/// movement AND weapon aim follow the view. Native look is starved during possession, so we are
/// the only writer (no contention). Element base = control_unit_slot() - CTL_ELEM_UNIT.
pub fn drive_control_look(fwd: [f32; 3]) {
    let slot = control_unit_slot();
    if slot == 0 {
        return;
    }
    let base = slot - CTL_ELEM_UNIT;
    let _ = crate::seh::guard(|| unsafe {
        let p = (base + CTL_ELEM_LOOK) as *mut f32;
        *p = fwd[0];
        *p.add(1) = fwd[1];
        *p.add(2) = fwd[2];
    });
}

// Biped facing-motor angular-velocity clamp: two .rdata f32 constants (±2.0944 rad/s = ±120°/s)
// that cap how fast body+0x50 turns toward desired-facing body+0x1D4. Referenced ONLY by the turn
// motor (0x62AD10), so widening them makes the possessed unit's facing SNAP to our aim (natural
// WASD, since movement is relative to body+0x50) without the body+0x50 direct-write freeze.
const TURN_CLAMP_POS_RVA: usize = 0x8f2be8; // f32 +2.0944 (max +angular velocity)
const TURN_CLAMP_NEG_RVA: usize = 0x8f317c; // f32 -2.0944 (max -angular velocity)
static TURN_CLAMP_SAVED: AtomicU32 = AtomicU32::new(0); // original +clamp f32 bits (0 = not saved)

/// Write an f32 into (normally read-only) .rdata, flipping page protection around the write.
unsafe fn write_rdata_f32(addr: *mut f32, val: f32) {
    let mut old = 0u32;
    if VirtualProtect(addr as *mut c_void, 4, PAGE_READWRITE, &mut old) != 0 {
        *addr = val;
        let mut tmp = 0u32;
        VirtualProtect(addr as *mut c_void, 4, old, &mut tmp);
    }
}

/// Widen the biped facing-motor clamp to `rad_per_s` (stock 2.0944; ~20 makes the body snap to our
/// desired-facing so WASD tracks the camera). Global while active (all bipeds turn faster); saves
/// the original on first call. Restore with [`restore_turn_clamp`] on release.
pub fn set_turn_clamp(rad_per_s: f32) {
    let sb = crate::mem::sim_base();
    if sb == 0 {
        return;
    }
    let _ = crate::seh::guard(|| unsafe {
        let pos = (sb + TURN_CLAMP_POS_RVA) as *mut f32;
        let neg = (sb + TURN_CLAMP_NEG_RVA) as *mut f32;
        if TURN_CLAMP_SAVED.load(Ordering::Relaxed) == 0 {
            TURN_CLAMP_SAVED.store((*pos).to_bits(), Ordering::Relaxed);
        }
        let v = rad_per_s.abs();
        write_rdata_f32(pos, v);
        write_rdata_f32(neg, -v);
    });
    crate::rep!("[turn] facing clamp -> +/-{rad_per_s:.1} rad/s");
}

/// Restore the stock biped facing-motor clamp (undo [`set_turn_clamp`]).
pub fn restore_turn_clamp() {
    let saved = TURN_CLAMP_SAVED.swap(0, Ordering::Relaxed);
    if saved == 0 {
        return;
    }
    let sb = crate::mem::sim_base();
    if sb == 0 {
        return;
    }
    let v = f32::from_bits(saved);
    let _ = crate::seh::guard(|| unsafe {
        write_rdata_f32((sb + TURN_CLAMP_POS_RVA) as *mut f32, v);
        write_rdata_f32((sb + TURN_CLAMP_NEG_RVA) as *mut f32, -v);
    });
    crate::rep!("[turn] facing clamp restored to stock");
}

/// Read the possessed unit's facing/aim vectors for movement-frame diagnosis:
/// (body+0x50 live forward, body+0x1D4 desired-facing, body+0x204 current-aim).
pub fn body_facing_diag(h: u32) -> Option<([f32; 3], [f32; 3], [f32; 3])> {
    let b = object_body(h);
    if b == 0 {
        return None;
    }
    let mut out = None;
    let _ = crate::seh::guard(|| unsafe {
        let r = |o: usize| {
            let p = (b + o) as *const f32;
            [*p, *p.add(1), *p.add(2)]
        };
        out = Some((r(0x50), r(0x1d4), r(0x204)));
    });
    out
}

/// Dump the control-element region (floats around the look vector) so the exact look-slot layout
/// can be confirmed against the values the sim/our writes leave there. Diagnostic only.
pub fn dump_control_element() {
    let slot = control_unit_slot();
    if slot == 0 {
        crate::rep!("[ctl] element unresolved");
        return;
    }
    let base = slot - CTL_ELEM_UNIT;
    let _ = crate::seh::guard(|| unsafe {
        let u = *((base + CTL_ELEM_UNIT) as *const u32);
        let f = |o: usize| *((base + o) as *const f32);
        crate::rep!(
            "[ctl] base=0x{base:x} unit=0x{u:08x} 84={:.2} 88={:.2} 8C={:.2} 90={:.2} 94={:.3} 98={:.3} 9C={:.3} A0={:.3} A4={:.3} A8={:.3}",
            f(0x84), f(0x88), f(0x8c), f(0x90), f(0x94), f(0x98), f(0x9c), f(0xa0), f(0xa4), f(0xa8)
        );
    });
}

// Weapon-fire bridge: the per-unit action bits already carry the player's trigger (the unit
// animates), but for a possessed unit the intent never reaches the weapon body's fire flags.
// Copy the primary-trigger bit into the weapon each frame so it actually discharges.
const ACTBITS_TLS_SLOT: usize = 0x3E0; // simTLS+0x3E0 -> per-unit action-bit array (u32, stride 4)
const ACT_PRIMARY_TRIGGER: u32 = 3; // bit index
const UNIT_WEAPON_SLOT: usize = 0x33E; // i8 active weapon slot (-1 = none)
const UNIT_WEAPON_ARRAY: usize = 0x344; // u32[] held-weapon handles by slot
const WPN_BARREL0_TRIGGER: usize = 0x29d; // u8, bit7 = trigger held

/// True if the unit's primary-trigger action bit is set (mirrors the game's own input mapping).
fn unit_primary_trigger(tls: usize, unit_handle: u32) -> bool {
    let mut held = false;
    let _ = crate::seh::guard(|| unsafe {
        let actbits = *((tls + ACTBITS_TLS_SLOT) as *const usize);
        if actbits != 0 {
            let uidx = (unit_handle & 0xffff) as usize;
            let bits = *((actbits + uidx * 4) as *const u32);
            held = (bits >> ACT_PRIMARY_TRIGGER) & 1 != 0;
        }
    });
    held
}

/// Dump the possessed unit's weapon chain (slot, handles, weapon body flags, action bits) so
/// the fire path can be diagnosed. Prints to the log.
pub fn diag_weapon(unit_handle: u32) {
    let tls = sim_tls_block();
    let ubody = object_body(unit_handle);
    crate::rep!("[dw] unit 0x{unit_handle:08x} body=0x{ubody:x} tls=0x{tls:x}");
    if ubody == 0 {
        return;
    }
    let _ = crate::seh::guard(|| unsafe {
        let slot = *((ubody + UNIT_WEAPON_SLOT) as *const i8);
        crate::rep!("[dw] active weapon slot={slot}");
        for s in 0..4usize {
            let wh = *((ubody + UNIT_WEAPON_ARRAY + s * 4) as *const u32);
            crate::rep!("[dw]   slot{s} handle=0x{wh:08x}");
        }
        if slot >= 0 {
            let wh = *((ubody + UNIT_WEAPON_ARRAY + slot as usize * 4) as *const u32);
            let wb = object_body(wh);
            crate::rep!("[dw] active wpn=0x{wh:08x} body=0x{wb:x}");
            if wb != 0 {
                let b1ba = *((wb + 0x1ba) as *const u16);
                let b29d = *((wb + 0x29d) as *const u8);
                crate::rep!("[dw] wpn +0x1BA=0x{b1ba:04x}  +0x29D=0x{b29d:02x}");
            }
        }
        let actbits = *((tls + ACTBITS_TLS_SLOT) as *const usize);
        let uidx = (unit_handle & 0xffff) as usize;
        let bits = if actbits != 0 { *((actbits + uidx * 4) as *const u32) } else { 0 };
        crate::rep!("[dw] actbits base=0x{actbits:x} [{uidx}]=0x{bits:08x} trigbit3={}", (bits >> 3) & 1);
    });
}

/// The possessed unit's forward vector (body+0x50).
pub fn body_forward(unit_handle: u32) -> Option<[f32; 3]> {
    let b = object_body(unit_handle);
    if b == 0 {
        return None;
    }
    let mut f = [0f32; 3];
    let ok = crate::seh::guard(|| unsafe {
        let q = (b + 0x50) as *const f32;
        f = [*q, *q.add(1), *q.add(2)];
    });
    if ok && f.iter().all(|x| x.is_finite()) {
        Some(f)
    } else {
        None
    }
}

/// True if `handle` still names a live object (its salt matches the object-table record). Guards
/// against writing to a body whose slot was freed/reused after the possessed unit died.
pub fn handle_live(handle: u32) -> bool {
    if handle == 0xffff_ffff {
        return false;
    }
    let table = object_table();
    if table == 0 {
        return false;
    }
    let mut live = false;
    let _ = crate::seh::guard(|| unsafe {
        let rec = table + (handle & 0xffff) as usize * OBJ_RECORD_STRIDE;
        let salt = *((rec + OBJ_REC_SALT) as *const u16);
        live = salt as u32 == (handle >> 16);
    });
    live
}

/// Turn the unit + aim its weapon toward `fwd` by writing the engine's own DESIRED-facing trio
/// (the fields object_set_facing @0x351600 writes to steer live AI bipeds) plus the current-aim
/// vector - and marking the object active. NEVER writes body+0x50 (the live physics basis, which
/// crashes/freezes when overwritten). Body validated first.
pub fn drive_unit_aim(unit_handle: u32, fwd: [f32; 3]) {
    let b = object_body(unit_handle);
    if b == 0 || !crate::simunit::valid_unit(b) {
        return;
    }
    let table = object_table();
    const OFF: [usize; 4] = [0x1d4, 0x1f8, 0x21c, 0x204]; // desired_facing/aiming/looking + current_aim
    let _ = crate::seh::guard(|| unsafe {
        for o in OFF {
            let p = (b + o) as *mut f32;
            *p = fwd[0];
            *p.add(1) = fwd[1];
            *p.add(2) = fwd[2];
        }
        if table != 0 {
            *((table + (unit_handle & 0xffff) as usize * OBJ_RECORD_STRIDE + 2) as *mut u8) |= 2;
        }
    });
}

/// Drive the unit's HORIZONTAL world velocity (body+0x68 vx, +0x6C vy, world units/sec) and wake
/// physics. Leaves +0x70 (vz) alone so gravity/jumping still integrate. The safe movement
/// primitive (object_set_velocity core @0x663B90 writes exactly these). Call every tick.
pub fn drive_unit_velocity(unit_handle: u32, vx: f32, vy: f32) {
    let b = object_body(unit_handle);
    if b == 0 || !crate::simunit::valid_unit(b) {
        return;
    }
    let table = object_table();
    let _ = crate::seh::guard(|| unsafe {
        let p = (b + 0x68) as *mut f32;
        *p = vx;
        *p.add(1) = vy;
        if table != 0 {
            *((table + (unit_handle & 0xffff) as usize * OBJ_RECORD_STRIDE + 2) as *mut u8) |= 2;
        }
    });
}

/// Base of a unit's AI actor struct: AI actor table (*(simTLS+0x28)+0x50, stride 0xD10) indexed by
/// the unit's AI datum (body+0x1AC & 0xffff). 0 if the unit has no AI. Callers add the field offset
/// (+0x32C braindead-state, +0x88/+0x89 animation-LOD, ...).
fn ai_actor_base(unit_handle: u32) -> usize {
    let body = object_body(unit_handle);
    if body == 0 {
        return 0;
    }
    let tls = sim_tls_block();
    if tls == 0 {
        return 0;
    }
    let mut addr = 0usize;
    let _ = crate::seh::guard(|| unsafe {
        let ai_h = *((body + 0x1ac) as *const u32);
        if ai_h == 0xffff_ffff {
            return;
        }
        let glob = *((tls + 0x28) as *const usize);
        if glob == 0 {
            return;
        }
        let atable = *((glob + 0x50) as *const usize);
        if atable == 0 {
            return;
        }
        addr = atable + (ai_h & 0xffff) as usize * 0xd10;
    });
    addr
}

/// Read/write the possessed unit's AI "braindead" state field (ai_actor+0x32C). Braindead = 0
/// (the AI stops driving the unit, so our injected velocity/aim/fire win). None if no AI.
fn ai_actor_state_ptr(unit_handle: u32) -> usize {
    let base = ai_actor_base(unit_handle);
    if base == 0 {
        return 0;
    }
    base + 0x32c
}

/// (diag) A unit's AI animation-LOD tier: ai_actor+0x89 (1 = full detail / limbs animate, 3 = low
/// LOD / limbs frozen), plus the override byte ai_actor+0x88. None if the unit has no AI actor.
/// SWARM8b: the frozen "other characters" near a possessed player should read +0x89 == 3.
pub fn ai_lod(unit_handle: u32) -> Option<(u8, u8)> {
    let base = ai_actor_base(unit_handle);
    if base == 0 {
        return None;
    }
    let mut out = None;
    let _ = crate::seh::guard(|| unsafe {
        out = Some((*((base + 0x89) as *const u8), *((base + 0x88) as *const u8)));
    });
    out
}

/// (diag) The sim-thread AI-globals "full-detail actor count" cap: *(sim_tls_block()+0x40)+0xa (u16).
pub fn ai_full_detail_count() -> Option<u16> {
    let tls = sim_tls_block();
    if tls == 0 {
        return None;
    }
    let mut out = None;
    let _ = crate::seh::guard(|| unsafe {
        let g = *((tls + 0x40) as *const usize);
        if g != 0 {
            out = Some(*((g + 0xa) as *const u16));
        }
    });
    out
}

/// Set the sim-thread AI-globals full-detail actor count (SWARM8b fix C). LOD-UP only (more actors
/// get full-detail animation), so it structurally cannot rig dead marines. Writing on the SIM thread
/// via sim_tls_block() is the point: the hs: verb wrote the GAME thread's block, which is why it was
/// inert. Returns the PREVIOUS value so the caller can restore it on release.
pub fn set_ai_full_detail_count(n: u16) -> Option<u16> {
    let tls = sim_tls_block();
    if tls == 0 {
        return None;
    }
    let mut prev = None;
    let _ = crate::seh::guard(|| unsafe {
        let g = *((tls + 0x40) as *const usize);
        if g != 0 {
            let p = (g + 0xa) as *mut u16;
            prev = Some(*p);
            *p = n;
        }
    });
    prev
}

/// (diag) The AI-LOD observer records: *(sim_tls_block()+0x188), 8 records of stride 0x3C. Each is
/// (slot_idx i16 at rec+0x0; position f32x3 at rec+0x28..0x30 = the LOD distance origin). Compare the
/// position to the possessed unit's body+0x44 to see whether the observer actually tracks us.
pub fn ai_lod_observers() -> Vec<(i16, [f32; 3])> {
    let mut out = Vec::new();
    let tls = sim_tls_block();
    if tls == 0 {
        return out;
    }
    let _ = crate::seh::guard(|| unsafe {
        let arr = *((tls + 0x188) as *const usize);
        if arr == 0 {
            return;
        }
        for i in 0..8usize {
            let rec = arr + i * 0x3c;
            let slot = *((rec + 0x0) as *const i16);
            let p = (rec + 0x28) as *const f32;
            out.push((slot, [*p, *p.add(1), *p.add(2)]));
        }
    });
    out
}

/// (diag) A unit's body position (body+0x44, f32x3).
pub fn unit_pos(unit_handle: u32) -> Option<[f32; 3]> {
    let body = object_body(unit_handle);
    if body == 0 {
        return None;
    }
    let mut out = None;
    let _ = crate::seh::guard(|| unsafe {
        let p = (body + 0x44) as *const f32;
        out = Some([*p, *p.add(1), *p.add(2)]);
    });
    out
}

/// Read the possessed unit's AI braindead-state field (ai_actor+0x32C), for diagnostics.
pub fn ai_state(unit_handle: u32) -> Option<i32> {
    let addr = ai_actor_state_ptr(unit_handle);
    if addr == 0 {
        return None;
    }
    let mut v = 0i32;
    let ok = crate::seh::guard(|| unsafe { v = *(addr as *const i32) });
    if ok {
        Some(v)
    } else {
        None
    }
}

/// The possessed unit's active weapon handle + barrel flags (+0x1BA, +0x29D), for diagnostics.
pub fn weapon_diag(unit_handle: u32) -> Option<(u32, u16, u8)> {
    let ubody = object_body(unit_handle);
    if ubody == 0 {
        return None;
    }
    let mut out = None;
    let _ = crate::seh::guard(|| unsafe {
        let slot = *((ubody + UNIT_WEAPON_SLOT) as *const i8);
        if slot < 0 {
            return;
        }
        let wh = *((ubody + UNIT_WEAPON_ARRAY + slot as usize * 4) as *const u32);
        if wh == 0xffff_ffff {
            return;
        }
        let wb = object_body(wh);
        if wb == 0 {
            return;
        }
        out = Some((wh, *((wb + 0x1ba) as *const u16), *((wb + 0x29d) as *const u8)));
    });
    out
}

/// Set the possessed unit's AI braindead field to `val`, returning the previous value.
pub fn set_ai_state(unit_handle: u32, val: i32) -> Option<i32> {
    let addr = ai_actor_state_ptr(unit_handle);
    if addr == 0 {
        return None;
    }
    let mut old = 0i32;
    let ok = crate::seh::guard(|| unsafe {
        let p = addr as *mut i32;
        old = *p;
        *p = val;
    });
    if ok {
        Some(old)
    } else {
        None
    }
}

/// Force one shot: drive barrel 0 straight to the FIRE state with an expired refire counter, so
/// the already-ticking weapon updater runs the engine's own emit block (projectile + ammo + fx).
/// Call once per intended shot (on a cadence) - NOT every frame, or it multi-fires.
pub fn force_fire_shot(unit_handle: u32) {
    let ubody = object_body(unit_handle);
    if ubody == 0 {
        return;
    }
    let table = object_table();
    let _ = crate::seh::guard(|| unsafe {
        let slot = *((ubody + UNIT_WEAPON_SLOT) as *const i8);
        if slot < 0 {
            return;
        }
        let wpn_h = *((ubody + UNIT_WEAPON_ARRAY + slot as usize * 4) as *const u32);
        if wpn_h == 0xffff_ffff {
            return;
        }
        let wbody = object_body(wpn_h);
        if wbody == 0 {
            return;
        }
        *((wbody + 0x29c) as *mut u8) = 1; // barrel 0 state = firing
        *((wbody + 0x2a0) as *mut u16) = 0; // refire counter 0 -> emit this tick
        if table != 0 {
            *((table + (wpn_h & 0xffff) as usize * OBJ_RECORD_STRIDE + 2) as *mut u8) |= 2;
        }
    });
}

/// The local player's index (low 16 of the player-0 handle), for owner writes.
pub fn local_player_index() -> Option<i16> {
    let tls = sim_tls_block();
    if tls == 0 {
        return None;
    }
    let mut idx = None;
    let _ = crate::seh::guard(|| unsafe {
        let pcg = *((tls + PCG_TLS_SLOT) as *const usize);
        if pcg != 0 {
            let ph = *((pcg + PCG_LOCAL0_HANDLE) as *const u32);
            if ph != 0xffff_ffff {
                idx = Some((ph & 0xffff) as i16);
            }
        }
    });
    idx
}

/// Stamp the possessing player index into the unit body (+0x1BC) so hit/impact feedback and the
/// game's player-context routing recognise it as player-driven. -1 (0xFFFF) on release.
pub fn set_unit_owner(unit_handle: u32, player_idx: i16) {
    let b = object_body(unit_handle);
    if b == 0 {
        return;
    }
    let _ = crate::seh::guard(|| unsafe { *((b + 0x1bc) as *mut i16) = player_idx });
}

static FIRE_HELD: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Drive the possessed unit's weapon: set/clear the barrel trigger-held flag from `firing`
/// (polled directly from the mouse by the caller, so it works regardless of input routing) and
/// mark the weapon object active for update. Returns Some(true) when it drove a shot.
pub fn drive_weapon_fire(unit_handle: u32, firing: bool) -> Option<bool> {
    let tls = sim_tls_block();
    if tls == 0 {
        return None;
    }
    let act_bit = unit_primary_trigger(tls, unit_handle); // the game's own trigger bit, for diag
    let ubody = object_body(unit_handle);
    if ubody == 0 {
        return None;
    }
    let table = object_table();
    let mut drove = None;
    let _ = crate::seh::guard(|| unsafe {
        let slot = *((ubody + UNIT_WEAPON_SLOT) as *const i8);
        if slot < 0 {
            return;
        }
        let wpn_h = *((ubody + UNIT_WEAPON_ARRAY + (slot as usize) * 4) as *const u32);
        if wpn_h == 0xffff_ffff {
            return;
        }
        let wbody = object_body(wpn_h);
        if wbody == 0 {
            return;
        }
        let p = (wbody + WPN_BARREL0_TRIGGER) as *mut u8;
        // Mirror LMB into the barrel trigger-held bit ONLY (do NOT also OR in the game's own
        // act_bit). A CHARGE weapon (plasma pistol) fires on the trigger-RELEASE edge; if act_bit
        // stays latched, bit7 never clears on LMB-up, so the barrel parks in the charging state and
        // never discharges. Gating on `firing` alone gives: hold = charge / auto-fire, release =
        // discharge. Works for both charge and automatic weapons.
        if firing {
            *p |= 0x80;
        } else {
            *p &= !0x80;
        }
        // Mark the weapon object active for update (replicates sim 0x59F460: record+2 |= 2).
        if table != 0 {
            let rec = table + (wpn_h & 0xffff) as usize * OBJ_RECORD_STRIDE;
            *((rec + 2) as *mut u8) |= 2;
        }
        drove = Some(firing);
    });
    // Log the trigger rising/falling edge (LMB state + the game's own action bit, for compare).
    if FIRE_HELD.swap(firing, Ordering::Relaxed) != firing {
        crate::rep!(
            "[dw] LMB {} (game act_bit={}, unit 0x{unit_handle:08x}) drove={:?}",
            if firing { "DOWN" } else { "up" },
            act_bit as u8,
            drove
        );
    }
    drove
}

/// The local player's current unit handle (None if unresolved / no unit).
pub fn player_unit_handle() -> Option<u32> {
    let s = player_unit_slot();
    if s == 0 {
        return None;
    }
    let mut h = 0xffff_ffffu32;
    let ok = crate::seh::guard(|| unsafe { h = *(s as *const u32) });
    if ok && h != 0xffff_ffff {
        Some(h)
    } else {
        None
    }
}

/// The sim object table base (record array), or 0 if unresolved.
fn object_table() -> usize {
    let tls = sim_tls_block();
    if tls == 0 {
        return 0;
    }
    let mut t = 0usize;
    crate::seh::guard(|| unsafe {
        let og = *((tls + OBJTABLE_TLS_SLOT) as *const usize);
        if og != 0 {
            t = *((og + OBJTABLE_BASE_OFF) as *const usize);
        }
    });
    t
}

static SALT_OFF: AtomicUsize = AtomicUsize::new(0); // record-relative salt offset, +1 (0 = unknown)

/// Calibrate the salt (identifier) offset within an object-table record by matching the
/// player's known salt in the player's own record. Cached.
fn salt_off() -> Option<usize> {
    let c = SALT_OFF.load(Ordering::Relaxed);
    if c != 0 {
        return Some(c - 1);
    }
    let ph = player_unit_handle()?;
    let pindex = (ph & 0xffff) as usize;
    let psalt = (ph >> 16) as u16;
    if psalt == 0 {
        return None; // can't calibrate against a zero salt
    }
    let table = object_table();
    if table == 0 {
        return None;
    }
    let mut off = None;
    crate::seh::guard(|| unsafe {
        let rec = table + pindex * OBJ_RECORD_STRIDE;
        for o in (0..OBJ_RECORD_BODY).step_by(2) {
            if *((rec + o) as *const u16) == psalt {
                off = Some(o);
                return;
            }
        }
    });
    match off {
        Some(o) => {
            SALT_OFF.store(o + 1, Ordering::Relaxed);
            crate::rep!("[possess] salt field @ record +0x{o:x} (player salt 0x{psalt:04x})");
            Some(o)
        }
        None => {
            crate::rep!("[possess] couldn't locate salt field (player handle 0x{ph:08x})");
            None
        }
    }
}

/// Find the handle of the unit body closest to `pos` (world coords). `pos_off`/`is_f64`
/// describe where a body stores its position (calibrated by the caller against the player).
/// Returns (handle, distance) for the nearest valid unit, or None.
pub fn unit_handle_near(pos: [f32; 3], pos_off: usize, is_f64: bool) -> Option<(u32, f32)> {
    let table = object_table();
    if table == 0 {
        return None;
    }
    let soff = salt_off()?;
    let mut best: Option<(u32, f32)> = None;
    crate::seh::guard(|| unsafe {
        for i in 0..2048usize {
            let rec = table + i * OBJ_RECORD_STRIDE;
            let body = *((rec + OBJ_RECORD_BODY) as *const usize);
            if body == 0 || !crate::simunit::valid_unit(body) {
                continue;
            }
            let bp = if is_f64 {
                let p = (body + pos_off) as *const f64;
                [*p as f32, *p.add(1) as f32, *p.add(2) as f32]
            } else {
                let p = (body + pos_off) as *const f32;
                [*p, *p.add(1), *p.add(2)]
            };
            let d = (bp[0] - pos[0]).powi(2) + (bp[1] - pos[1]).powi(2) + (bp[2] - pos[2]).powi(2);
            if best.map_or(true, |(_, bd)| d < bd) {
                let salt = *((rec + soff) as *const u16);
                best = Some((((salt as u32) << 16) | (i as u32), d));
            }
        }
    });
    best.map(|(h, d)| (h, d.sqrt()))
}

// --- Sim object BODY offsets (static analysis of sim accessors; docs/research possess RE) ---
pub const BODY_POS: usize = 0x44; // f32[3] position, Blam world units
pub const BODY_AI_REF: usize = 0x1ac; // u32 AI datum handle (0xFFFFFFFF = no AI / player-driven)
pub const OBJ_REC_SALT: usize = 0x00; // u16 salt at object-table record+0 (confirmed @ sim 0x5A9690)
/// 1 Blam world unit = 3.048 m = 304.8 UE units (sim const 3.048 @ rva 0x8F299C).
pub const WU_TO_UU: f32 = 304.8;

/// The local player's sim body position (body+0x44, world units), or None.
pub fn player_sim_pos() -> Option<[f32; 3]> {
    let u = player_unit();
    if !crate::simunit::valid_unit(u) {
        return None;
    }
    let mut p = [0f32; 3];
    let ok = crate::seh::guard(|| unsafe {
        let q = (u + BODY_POS) as *const f32;
        p = [*q, *q.add(1), *q.add(2)];
    });
    if ok && p.iter().all(|x| x.is_finite()) {
        Some(p)
    } else {
        None
    }
}

/// Nearest valid unit body to a sim-space point; returns (salted handle, distance wu, unit count).
/// Position is the known body+0x44 (f32); salt is record+0. No offset calibration needed.
pub fn unit_handle_near_pos(pos: [f32; 3]) -> Option<(u32, f32, u32)> {
    let table = object_table();
    if table == 0 {
        return None;
    }
    let mut best: Option<(u32, f32)> = None;
    let mut count = 0u32;
    crate::seh::guard(|| unsafe {
        for i in 0..2048usize {
            let rec = table + i * OBJ_RECORD_STRIDE;
            let body = *((rec + OBJ_RECORD_BODY) as *const usize);
            if body == 0 || !crate::simunit::valid_unit(body) {
                continue;
            }
            count += 1;
            let p = (body + BODY_POS) as *const f32;
            let bp = [*p, *p.add(1), *p.add(2)];
            let d2 = (bp[0] - pos[0]).powi(2) + (bp[1] - pos[1]).powi(2) + (bp[2] - pos[2]).powi(2);
            if best.map_or(true, |(_, bd)| d2 < bd * bd) {
                let salt = *((rec + OBJ_REC_SALT) as *const u16);
                best = Some((((salt as u32) << 16) | (i as u32), d2.sqrt()));
            }
        }
    });
    best.map(|(h, d)| (h, d, count))
}

/// The body's normalized (body, shield) vitality from vitality records 0 and 1 (NaN if absent).
pub fn body_vitality(body: usize) -> (f32, f32) {
    let mut v = (f32::NAN, f32::NAN);
    crate::seh::guard(|| unsafe {
        let off = *((body + 0x176) as *const u16) as usize; // vitality-array byte offset
        let base = body + off;
        v.0 = *((base + 0x10) as *const f32); // record 0 = body
        v.1 = *((base + 0x18 + 0x10) as *const f32); // record 1 = shield
    });
    v
}

/// Given a raw value that is either a full datum handle or a bare index, return the full salted
/// handle if it names a valid unit record, else None.
pub fn normalize_to_full_handle(raw: u32) -> Option<u32> {
    let table = object_table();
    if table == 0 {
        return None;
    }
    let idx = (raw & 0xffff) as usize;
    let mut out = None;
    crate::seh::guard(|| unsafe {
        let rec = table + idx * OBJ_RECORD_STRIDE;
        let body = *((rec + OBJ_RECORD_BODY) as *const usize);
        if body != 0 && crate::simunit::valid_unit(body) {
            let salt = *((rec + OBJ_REC_SALT) as *const u16);
            out = Some(((salt as u32) << 16) | (idx as u32));
        }
    });
    out
}

/// Find the full Blam datum handle (salt<<16 | index) for a given object body pointer by
/// scanning the object table for the record whose body matches. None if not found.
pub fn handle_for_body(body: usize) -> Option<u32> {
    if body == 0 {
        return None;
    }
    let table = object_table();
    if table == 0 {
        return None;
    }
    let soff = salt_off()?;
    let mut result = None;
    crate::seh::guard(|| unsafe {
        for i in 0..4096usize {
            let rec = table + i * OBJ_RECORD_STRIDE;
            if *((rec + OBJ_RECORD_BODY) as *const usize) == body {
                let salt = *((rec + soff) as *const u16);
                result = Some(((salt as u32) << 16) | (i as u32));
                return;
            }
        }
    });
    result
}

/// Overwrite the local player's unit handle, returning the previous handle (None if the
/// chain could not be resolved). This is sim-side possession: the sim routes player movement
/// and weapon input to whatever unit this slot names. Reversible via the returned handle.
pub fn set_player_unit_handle(new_h: u32) -> Option<u32> {
    let s = player_unit_slot();
    if s == 0 {
        return None;
    }
    let mut old = None;
    crate::seh::guard(|| unsafe {
        let p = s as *mut u32;
        old = Some(*p);
        *p = new_h;
    });
    old
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
