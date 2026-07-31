//! Keyframed camera paths: capture POVs and play a Catmull-Rom spline through them.

use std::sync::Mutex;

use crate::state::cam;

#[derive(Clone, Copy, Default)]
struct Kf {
    x: f64,
    y: f64,
    z: f64,
    pitch: f64,
    yaw: f64,
    roll: f64,
    fov: f32,
}

struct PathState {
    kf: Vec<Kf>,
    playing: bool,
    t: f64,
    dur: f32,
}

static PATH: Mutex<PathState> = Mutex::new(PathState {
    kf: Vec::new(),
    playing: false,
    t: 0.0,
    dur: 8.0,
});

fn lock() -> std::sync::MutexGuard<'static, PathState> {
    PATH.lock().unwrap_or_else(|e| e.into_inner())
}

fn catmull_rom(p0: f64, p1: f64, p2: f64, p3: f64, u: f64) -> f64 {
    let u2 = u * u;
    let u3 = u2 * u;
    0.5 * (2.0 * p1
        + (-p0 + p2) * u
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * u2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * u3)
}

/// Number of captured keyframes.
pub fn count() -> usize {
    lock().kf.len()
}

/// (keyframe count, playhead 0..1, playing) for the timeline widget.
pub fn timeline() -> (usize, f64, bool) {
    let p = lock();
    (p.kf.len(), p.t, p.playing)
}

/// Every keyframe as `(x, y, z, pitch, yaw, roll, fov)`, for the keyframe list window.
pub fn keyframes() -> Vec<(f64, f64, f64, f64, f64, f64, f32)> {
    lock()
        .kf
        .iter()
        .map(|k| (k.x, k.y, k.z, k.pitch, k.yaw, k.roll, k.fov))
        .collect()
}

/// Capture the current camera POV as a keyframe (max 32).
pub fn add() {
    let s = cam();
    let kf = Kf {
        x: s.x,
        y: s.y,
        z: s.z,
        pitch: s.pitch,
        yaw: s.yaw,
        roll: s.roll,
        fov: s.fov,
    };
    drop(s);
    let mut p = lock();
    if p.kf.len() < 32 {
        p.kf.push(kf);
        let n = p.kf.len();
        drop(p);
        crate::rep!("[path] keyframe {} captured", n);
    }
}

/// Clear all keyframes and stop playback.
pub fn clear() {
    let mut p = lock();
    p.kf.clear();
    p.playing = false;
    drop(p);
    crate::rep!("[path] cleared");
}

/// Start or stop spline playback (needs at least 2 keyframes).
pub fn toggle_play() {
    let mut p = lock();
    if p.kf.len() < 2 {
        drop(p);
        crate::rep!("[path] need at least 2 keyframes");
        return;
    }
    p.playing = !p.playing;
    p.t = 0.0;
    let on = p.playing;
    drop(p);
    if on {
        cam().freecam = true; // playback drives the free-cam POV
    }
    crate::rep!("[path] playback {}", if on { "start" } else { "stop" });
}

/// Advance playback one tick and write the interpolated POV into the camera state.
/// Call every frame from the input loop.
pub fn update() {
    let mut p = lock();
    if !p.playing || p.kf.len() < 2 {
        return;
    }
    p.t += 0.015 / p.dur as f64;
    if p.t >= 1.0 {
        p.t = 1.0;
        p.playing = false;
    }
    let n = p.kf.len();
    let s = p.t * (n - 1) as f64;
    let i = (s as usize).min(n - 2);
    let u = s - i as f64;
    let at = |idx: isize| -> Kf {
        let j = idx.clamp(0, (n - 1) as isize) as usize;
        p.kf[j]
    };
    let (k0, k1, k2, k3) = (
        at(i as isize - 1),
        at(i as isize),
        at(i as isize + 1),
        at(i as isize + 2),
    );
    let done = !p.playing;
    drop(p);

    let mut s = cam();
    s.x = catmull_rom(k0.x, k1.x, k2.x, k3.x, u);
    s.y = catmull_rom(k0.y, k1.y, k2.y, k3.y, u);
    s.z = catmull_rom(k0.z, k1.z, k2.z, k3.z, u);
    s.pitch = catmull_rom(k0.pitch, k1.pitch, k2.pitch, k3.pitch, u);
    s.yaw = catmull_rom(k0.yaw, k1.yaw, k2.yaw, k3.yaw, u);
    s.roll = catmull_rom(k0.roll, k1.roll, k2.roll, k3.roll, u);
    s.fov = catmull_rom(k0.fov as f64, k1.fov as f64, k2.fov as f64, k3.fov as f64, u) as f32;
    drop(s);
    if done {
        crate::rep!("[path] playback complete");
    }
}
