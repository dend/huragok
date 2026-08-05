//! Watched command file. Polls `huragok_cmds.txt` next to the game exe and, whenever it
//! changes on disk, feeds each line into the console queue (same path as typing into the
//! in-game console). This lets commands be authored from outside the game - write the file,
//! the mod runs it within a poll or two - so testing HaloScript needs no in-game typing.
//!
//! Format: one command per line; blank lines and lines starting with `#` are ignored.
//! `hs:` lines and `huragok ...` builtins both work (they route through run_console_line).

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;

const FILE: &str = "huragok_cmds.txt";

static LAST_MTIME: AtomicU64 = AtomicU64::new(0); // last-seen modified time (nanos; 0 = none)
static PRIMED: AtomicBool = AtomicBool::new(false);
static TICK: AtomicUsize = AtomicUsize::new(0);

fn path() -> Option<std::path::PathBuf> {
    Some(crate::log::exe_dir()?.join(FILE))
}

fn mtime_nanos() -> u64 {
    let Some(p) = path() else { return 0 };
    match std::fs::metadata(&p).and_then(|m| m.modified()) {
        Ok(t) => t.duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0),
        Err(_) => 0,
    }
}

/// Record the current file state without running it, so a stale file left from a previous
/// session does not auto-execute on load. Call once at startup.
pub fn init() {
    LAST_MTIME.store(mtime_nanos(), Ordering::Relaxed);
    PRIMED.store(true, Ordering::Relaxed);
    if let Some(p) = path() {
        crate::rep!("[script] watching {} - write commands there to run them", p.display());
    }
}

/// Poll for a change and run the file if it moved. Call from the worker loop; self-throttles
/// to roughly twice a second so we are not stat-ing the disk every 15 ms.
pub fn poll() {
    if !PRIMED.load(Ordering::Relaxed) {
        return;
    }
    if TICK.fetch_add(1, Ordering::Relaxed) % 30 != 0 {
        return;
    }
    let mt = mtime_nanos();
    if mt != 0 && mt != LAST_MTIME.load(Ordering::Relaxed) {
        LAST_MTIME.store(mt, Ordering::Relaxed);
        run();
    }
}

/// Re-read and run every line right now, regardless of mtime (for a force-run hotkey).
pub fn run_now() {
    LAST_MTIME.store(mtime_nanos(), Ordering::Relaxed);
    run();
}

fn run() {
    let Some(p) = path() else { return };
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) => {
            crate::rep!("[script] read failed: {e}");
            return;
        }
    };
    let mut n = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        crate::console::submit(line.to_string());
        n += 1;
    }
    crate::rep!("[script] queued {n} line(s) from {FILE}");
}
