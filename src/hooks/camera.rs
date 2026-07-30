//! Free-cam: detour the camera manager's `ProcessEvent` and override the POV that
//! `BlueprintUpdateCamera` produces each frame.

use core::cell::Cell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use crate::offsets::*;
use crate::state::cam;
use crate::ue::fname::obj_name;
use crate::ue::reflect::{class_of, find_function, find_live_by_class};

type PeFn = unsafe extern "system" fn(*mut u8, *mut u8, *mut c_void);
type GetFovFn = unsafe extern "system" fn(*mut u8) -> f32;

static CAM_PE: AtomicUsize = AtomicUsize::new(0); // original ProcessEvent for the cam-mgr class
static ORIG_GETFOV: AtomicUsize = AtomicUsize::new(0); // original GetFOVAngle
static BP_UPDATE: AtomicUsize = AtomicUsize::new(0);
static F_GET_LOC: AtomicUsize = AtomicUsize::new(0);
static F_GET_ROT: AtomicUsize = AtomicUsize::new(0);
static F_GET_FOV: AtomicUsize = AtomicUsize::new(0);
static HOOKED: AtomicBool = AtomicBool::new(false);
static ALIVE: AtomicBool = AtomicBool::new(false);
static SAW_UPDATE: AtomicBool = AtomicBool::new(false);
static FAULTED: AtomicBool = AtomicBool::new(false);
static ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
// Lock-free FOV state for the GetFOVAngle detour, so it never touches the cam() Mutex
// (which the detour already holds on the same thread -> would self-deadlock).
static FORCED_FOV: AtomicU32 = AtomicU32::new(0x42b4_0000); // 90.0f32 bits
static FOV_ACTIVE: AtomicBool = AtomicBool::new(false);

thread_local! {
    static IN_CAM: Cell<bool> = const { Cell::new(false) };
}

/// Whether the camera hook is installed (or gave up trying).
pub fn installed() -> bool {
    HOOKED.load(Ordering::Relaxed)
}

/// Find the live camera manager, resolve its functions, and detour slot 79.
pub fn install() {
    if HOOKED.load(Ordering::Relaxed) {
        return;
    }
    // The gameplay camera is BP_BlamCameraManager_C. A base PlayerCameraManager also
    // exists early (menu / loading) but goes idle, so do not settle for it: wait for
    // the Blam one, and only fall back to a base manager after a long wait.
    let n = ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    let mut cam_mgr = find_live_by_class("BlamCameraManager");
    if cam_mgr.is_null() {
        if n == 200 {
            crate::rep!("[camhook] waiting for BP_BlamCameraManager_C to spawn (load into gameplay)");
        }
        if n < 1300 {
            return; // ~20s; keep waiting for the real gameplay camera
        }
        cam_mgr = find_live_by_class("CameraManager"); // last resort
        if cam_mgr.is_null() {
            return;
        }
        crate::rep!("[camhook] no Blam camera manager found; falling back to a base one");
    }
    unsafe {
        let cls = class_of(cam_mgr);
        let bp = find_function(cls, "BlueprintUpdateCamera");
        if bp.is_null() {
            crate::rep!("[camhook] BlueprintUpdateCamera not found on {}", obj_name(cls));
            HOOKED.store(true, Ordering::Relaxed); // stop retrying; wrong target
            return;
        }
        BP_UPDATE.store(bp as usize, Ordering::Relaxed);
        F_GET_LOC.store(find_function(cls, "GetCameraLocation") as usize, Ordering::Relaxed);
        F_GET_ROT.store(find_function(cls, "GetCameraRotation") as usize, Ordering::Relaxed);
        F_GET_FOV.store(find_function(cls, "GetFOVAngle") as usize, Ordering::Relaxed);

        // Store the original ProcessEvent BEFORE swapping, so the detour never runs
        // with CAM_PE == 0 (the install race).
        let vt = *(cam_mgr as *const *mut usize);
        CAM_PE.store(*vt.add(PROCESSEVENT_SLOT), Ordering::Relaxed);

        let detour: PeFn = cam_detour;
        super::hook_vtable_slot(cam_mgr, PROCESSEVENT_SLOT, detour as usize);

        // Also detour GetFOVAngle (read-time FOV) so a forced FOV survives Blam's
        // per-frame POV.FOV recompute, which discards our direct 0x3B0 write.
        let vt2 = *(cam_mgr as *const *mut usize);
        ORIG_GETFOV.store(*vt2.add(GETFOVANGLE_SLOT), Ordering::Relaxed);
        let getfov: GetFovFn = get_fov_detour;
        super::hook_vtable_slot(cam_mgr, GETFOVANGLE_SLOT, getfov as usize);

        HOOKED.store(true, Ordering::Relaxed);
        crate::rep!(
            "[camhook] {} ({}) slot {} detoured. INSERT = free-cam.",
            obj_name(cam_mgr),
            obj_name(cls),
            PROCESSEVENT_SLOT
        );
    }
}

/// Read-time FOV override: return our FOV when free-cam or the lock is active,
/// otherwise the game's real value.
unsafe extern "system" fn get_fov_detour(this: *mut u8) -> f32 {
    // Lock-free read. Skip our value while we are seeding (IN_CAM) so the seed adopts
    // the game's real FOV, and never lock cam() here (that would deadlock the detour).
    if !IN_CAM.with(|c| c.get()) && FOV_ACTIVE.load(Ordering::Relaxed) {
        return f32::from_bits(FORCED_FOV.load(Ordering::Relaxed));
    }
    let orig: GetFovFn = core::mem::transmute(ORIG_GETFOV.load(Ordering::Relaxed));
    orig(this)
}

unsafe extern "system" fn cam_detour(self_: *mut u8, func: *mut u8, parms: *mut c_void) {
    let orig: PeFn = core::mem::transmute(CAM_PE.load(Ordering::Relaxed));
    orig(self_, func, parms); // let the BP compute its POV first

    if !ALIVE.swap(true, Ordering::Relaxed) {
        crate::rep!("[camhook] detour live (first ProcessEvent seen).");
    }

    // Everything below dereferences game memory at RE'd offsets - guard it so a
    // wrong offset logs once instead of taking the whole game down.
    let ok = crate::seh::guard(|| cam_override(self_, func, parms, orig));
    if !ok && !FAULTED.swap(true, Ordering::Relaxed) {
        crate::rep!("[camhook] override faulted (guarded) - offsets likely off");
    }
}

fn cam_override(self_: *mut u8, func: *mut u8, parms: *mut c_void, orig: PeFn) {
    unsafe {
        if IN_CAM.with(|c| c.get()) {
            return;
        }
        if func as usize != BP_UPDATE.load(Ordering::Relaxed) || parms.is_null() {
            return;
        }
        if !SAW_UPDATE.swap(true, Ordering::Relaxed) {
            crate::rep!("[camhook] BlueprintUpdateCamera firing - override live.");
        }

        // Per-frame holds: re-assert the Blam sim time scale (the sim rewrites tick_length
        // each tick) and the third-person body / forced scale after the pawn tick.
        crate::pawn::hold_time();
        crate::pawn::hold_pawn_state();

        let mut st = cam();

        // Force POV.FOV: always in free-cam, otherwise only when the user locked it.
        // Blam ignores the BlueprintUpdateCamera FOV out-param, so write the camera
        // manager's ViewTarget.POV.FOV (0x3B0) directly each frame.
        // Publish FOV to the lock-free atomics the GetFOVAngle detour reads.
        let fov_active = st.freecam || st.fov_locked;
        FOV_ACTIVE.store(fov_active, Ordering::Relaxed);
        FORCED_FOV.store(st.fov.to_bits(), Ordering::Relaxed);
        if fov_active {
            *((self_ as usize + CAMMGR_POV_FOV) as *mut f32) = st.fov;
            *((self_ as usize + CAMMGR_POV_DESIRED_FOV) as *mut f32) = st.fov;
        }

        if !st.freecam {
            return;
        }

        if st.seed {
            IN_CAM.with(|c| c.set(true));
            let mut loc = [0f64; 3];
            let mut rot = [0f64; 3];
            let mut fov = 90f32;
            let gl = F_GET_LOC.load(Ordering::Relaxed) as *mut u8;
            let gr = F_GET_ROT.load(Ordering::Relaxed) as *mut u8;
            let gf = F_GET_FOV.load(Ordering::Relaxed) as *mut u8;
            if !gl.is_null() {
                orig(self_, gl, loc.as_mut_ptr() as *mut c_void);
            }
            if !gr.is_null() {
                orig(self_, gr, rot.as_mut_ptr() as *mut c_void);
            }
            if !gf.is_null() {
                orig(self_, gf, &mut fov as *mut f32 as *mut c_void);
            }
            IN_CAM.with(|c| c.set(false));
            st.x = loc[0];
            st.y = loc[1];
            st.z = loc[2];
            st.pitch = rot[0];
            st.yaw = rot[1];
            st.roll = rot[2];
            st.fov = fov;
            st.seed = false;
            crate::rep!(
                "[freecam] seed loc=({:.0},{:.0},{:.0}) rot=({:.1},{:.1},{:.1}) fov={:.0}",
                st.x, st.y, st.z, st.pitch, st.yaw, st.roll, st.fov
            );
        }

        let p = parms as *mut u8;
        *(p.add(BUC_LOCATION) as *mut f64) = st.x;
        *(p.add(BUC_LOCATION + 8) as *mut f64) = st.y;
        *(p.add(BUC_LOCATION + 16) as *mut f64) = st.z;
        *(p.add(BUC_ROTATION) as *mut f64) = st.pitch;
        *(p.add(BUC_ROTATION + 8) as *mut f64) = st.yaw;
        *(p.add(BUC_ROTATION + 16) as *mut f64) = st.roll;
        *(p.add(BUC_FOV) as *mut f32) = st.fov;
        *(p.add(BUC_RETURN) as *mut u8) = 1; // force the engine to use our POV
    }
}
