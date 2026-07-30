//! Command queue. UI and input threads push commands; the PlayerController
//! ProcessEvent detour drains them on the game thread (see hooks/pc.rs).

use core::sync::atomic::{AtomicUsize, Ordering};
use std::collections::VecDeque;
use std::sync::Mutex;

/// A queued action, run on the game thread against the PlayerController / pawn.
#[derive(Clone, Copy, Debug)]
pub enum Cmd {
    CineOn,
    CineOff,
    Pause,
    Freeze,
    Unfreeze,
    ImguiInput,
    Ghost,
    Fly,
    Walk,
    God,
    Ungod,
    Fov(f32),
    PawnHide,
    PawnShow,
    PawnNoCol,
    PawnCol,
    CamoOn,
    CamoOff,
    CamoToggle,
    OvershieldOn,
    OvershieldOff,
    FreezePawn,
    UnfreezePawn,
    ShieldBreak,
    NightVis,
    RechargePP,
    BloodHuman,
    BloodCov,
    BloodGrunt,
    BloodBrute,
    ScaleGiant,
    ScaleTiny,
    ScaleNormal,
    Teleport,
    RadialBlur,
    Breath,
    FullbodyOn,
    FullbodyOff,
    Slomo(f32),
    FadeOut,
    FadeIn,
}

static QUEUE: Mutex<VecDeque<Cmd>> = Mutex::new(VecDeque::new());
static PENDING: AtomicUsize = AtomicUsize::new(0); // lock-free count for the hot-path check

/// Enqueue a command (any thread).
pub fn push(c: Cmd) {
    if let Ok(mut q) = QUEUE.lock() {
        q.push_back(c);
        PENDING.fetch_add(1, Ordering::Relaxed);
    }
}

/// True if any commands are waiting. Lock-free, safe to call every ProcessEvent.
pub fn has_pending() -> bool {
    PENDING.load(Ordering::Relaxed) > 0
}

/// Take and clear all queued commands.
pub fn take_all() -> Vec<Cmd> {
    match QUEUE.lock() {
        Ok(mut q) => {
            let v: Vec<Cmd> = q.drain(..).collect();
            PENDING.fetch_sub(v.len(), Ordering::Relaxed);
            v
        }
        Err(_) => Vec::new(),
    }
}
