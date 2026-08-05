//! Shared "possession look" direction (yaw/pitch in radians), written by the input thread from
//! the mouse and read by the camera detour (orbit) and the game-thread control tick (unit aim).
//! One source of truth so the camera, the unit's facing, and the shot direction always agree.

use core::sync::atomic::{AtomicU32, Ordering};

static YAW: AtomicU32 = AtomicU32::new(0); // radians, f32 bits
static PITCH: AtomicU32 = AtomicU32::new(0);

fn ld(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}
fn st(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

/// Set the look direction absolutely (e.g. seed from the possessed unit's forward on entry).
pub fn seed(yaw: f32, pitch: f32) {
    st(&YAW, yaw);
    st(&PITCH, pitch.clamp(-1.40, 1.40));
}

/// Apply a mouse delta (radians). Pitch is clamped to just under +/- 90 degrees.
pub fn add(dyaw: f32, dpitch: f32) {
    st(&YAW, ld(&YAW) + dyaw);
    st(&PITCH, (ld(&PITCH) - dpitch).clamp(-1.40, 1.40));
}

pub fn yaw_pitch_rad() -> (f32, f32) {
    (ld(&YAW), ld(&PITCH))
}

pub fn yaw_pitch_deg() -> (f64, f64) {
    (ld(&YAW).to_degrees() as f64, ld(&PITCH).to_degrees() as f64)
}

/// Forward unit vector from the current look (Blam/freecam convention: x=cos p cos y, ...).
pub fn forward() -> [f32; 3] {
    let (y, p) = yaw_pitch_rad();
    let cp = p.cos();
    [cp * y.cos(), cp * y.sin(), p.sin()]
}
