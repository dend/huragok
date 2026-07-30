//! Shared engine state (camera POV + flags), written by the input thread and read
//! by the camera detour on the game thread.

use std::sync::{Mutex, MutexGuard};

/// Free-cam point of view and toggles.
pub struct CamData {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub pitch: f64,
    pub yaw: f64,
    pub roll: f64,
    pub fov: f32,
    pub fov_locked: bool, // force POV.FOV even outside free-cam (until reset)
    pub freecam: bool,
    pub seed: bool, // adopt the real current POV on the next frame
    pub mouse: bool,
}

static STATE: Mutex<CamData> = Mutex::new(CamData {
    x: 0.0,
    y: 0.0,
    z: 0.0,
    pitch: 0.0,
    yaw: 0.0,
    roll: 0.0,
    fov: 90.0,
    fov_locked: false,
    freecam: false,
    seed: false,
    mouse: false,
});

/// Lock the shared camera state (recovers from a poisoned lock).
pub fn cam() -> MutexGuard<'static, CamData> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}
