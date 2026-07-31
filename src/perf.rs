//! Engine-reported FPS for the stats graph.
//!
//! Reads UE's `GAverageFPS` global directly - the exact value `stat fps` shows, which the
//! engine formats with `"%5.2f FPS"`. The global's address was found by xref of that
//! format string. It is a plain memory read (no ProcessEvent), so it is safe to sample
//! from any thread, including the ImGui draw hook.

use std::sync::Mutex;

use crate::mem::base;
use crate::offsets::GAVERAGEFPS;

/// Ring-buffer length for the sparkline.
pub const SAMPLES: usize = 120;

struct Ring {
    cur: f32,
    ring: [f32; SAMPLES],
    head: usize,
    filled: usize,
}
static RING: Mutex<Ring> = Mutex::new(Ring {
    cur: 0.0,
    ring: [0.0; SAMPLES],
    head: 0,
    filled: 0,
});

/// Sample the engine FPS. Call once per rendered frame.
pub fn tick() {
    let b = base();
    if b == 0 {
        return;
    }
    let mut fps = 0.0f32;
    let ok = crate::seh::guard(|| unsafe {
        fps = *((b + GAVERAGEFPS) as *const f32);
    });
    if !ok || !fps.is_finite() || !(0.0..=100_000.0).contains(&fps) {
        return;
    }
    let mut r = RING.lock().unwrap_or_else(|e| e.into_inner());
    r.cur = fps;
    let h = r.head;
    r.ring[h] = fps;
    r.head = (h + 1) % SAMPLES;
    if r.filled < SAMPLES {
        r.filled += 1;
    }
}

/// Latest engine FPS (the engine already smooths it).
pub fn current() -> f32 {
    RING.lock().map(|r| r.cur).unwrap_or(0.0)
}

/// Copy the ring buffer oldest-to-newest into `out`; returns the sample count.
pub fn samples(out: &mut [f32; SAMPLES]) -> usize {
    let r = RING.lock().unwrap_or_else(|e| e.into_inner());
    let n = r.filled;
    let start = if r.filled < SAMPLES { 0 } else { r.head };
    for i in 0..n {
        out[i] = r.ring[(start + i) % SAMPLES];
    }
    n
}
