//! Detour the PlayerController's ProcessEvent to drain the command queue on the
//! game thread (commands need to run there, not on our worker thread).

use core::cell::Cell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::offsets::PROCESSEVENT_SLOT;
use crate::ue::fname::obj_name;
use crate::ue::reflect::class_of;

type PeFn = unsafe extern "system" fn(*mut u8, *mut u8, *mut c_void);

static PC_PE: AtomicUsize = AtomicUsize::new(0);
static HOOKED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static IN_DRAIN: Cell<bool> = const { Cell::new(false) };
}

/// Detour slot 79 on the PlayerController's class vtable.
pub fn install(pc: *mut u8) {
    if HOOKED.load(Ordering::Relaxed) || pc.is_null() {
        return;
    }
    unsafe {
        let vt = *(pc as *const *mut usize);
        PC_PE.store(*vt.add(PROCESSEVENT_SLOT), Ordering::Relaxed);
        let detour: PeFn = pc_detour;
        super::hook_vtable_slot(pc, PROCESSEVENT_SLOT, detour as usize);
        HOOKED.store(true, Ordering::Relaxed);
        crate::rep!(
            "[hook] PC {} slot {} detoured; command queue live.",
            obj_name(class_of(pc)),
            PROCESSEVENT_SLOT
        );
    }
}

static STAT_TICK: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn pc_detour(self_: *mut u8, func: *mut u8, parms: *mut c_void) {
    if !IN_DRAIN.with(|d| d.get()) {
        if crate::cmd::has_pending() {
            IN_DRAIN.with(|d| d.set(true));
            for c in crate::cmd::take_all() {
                crate::pawn::execute(self_, c);
            }
            IN_DRAIN.with(|d| d.set(false));
        }
        // Console input lines (typed into the log console) run as console commands.
        let lines = crate::console::take_all();
        if !lines.is_empty() {
            IN_DRAIN.with(|d| d.set(true));
            for line in lines {
                crate::pawn::run_console_line(self_, &line);
            }
            IN_DRAIN.with(|d| d.set(false));
        }
        // Refresh live stats / campaign readout on the game thread, throttled (ProcessEvent
        // fires often). Campaign changes rarely, so refresh it less frequently.
        let t = STAT_TICK.fetch_add(1, Ordering::Relaxed);
        if t % 64 == 0 {
            IN_DRAIN.with(|d| d.set(true));
            crate::stats::refresh(self_);
            if t % 256 == 0 {
                crate::campaign::refresh(self_);
            }
            IN_DRAIN.with(|d| d.set(false));
        }
        // Cheat maintenance: re-assert invulnerability so the sim cannot clear the flag.
        // Cheap no-op while the cheat is off; SEH-guarded internally.
        if t % 8 == 0 {
            IN_DRAIN.with(|d| d.set(true));
            crate::simunit::tick();
            IN_DRAIN.with(|d| d.set(false));
        }
    }
    let orig: PeFn = core::mem::transmute(PC_PE.load(Ordering::Relaxed));
    orig(self_, func, parms);
}
