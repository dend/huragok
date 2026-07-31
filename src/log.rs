//! Console + file logging with a clean, Claude-Code-style presentation.
//!
//! Every line is `[tag] message`; the console renders it as a colour-coded bullet
//! plus tag, collapses consecutive duplicates, and mirrors plain text to
//! `huragok_log.txt` next to the game exe. Use the [`rep!`](crate::rep) macro.

use core::ffi::c_void;
use core::sync::atomic::{AtomicPtr, Ordering};
use std::io::Write;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, WriteFile, OPEN_EXISTING};
use windows_sys::Win32::System::Console::{
    AllocConsole, GetConsoleMode, SetConsoleMode, SetConsoleOutputCP, SetConsoleTitleW,
    SetStdHandle, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
};

const RESET: &str = "\x1b[0m";
const ACCENT: &str = "\x1b[38;2;215;119;87m"; // Claude accent (tan/orange)
const DIM: &str = "\x1b[38;2;128;128;128m";
const BOLD: &str = "\x1b[1m";

static CONOUT: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());

// Recent plain-text log lines, mirrored for the in-game ImGui console.
static LINES: Mutex<std::collections::VecDeque<String>> =
    Mutex::new(std::collections::VecDeque::new());

/// Copy the last `max` log lines (oldest-first) into `out`, for the ImGui console.
pub fn recent(out: &mut Vec<String>, max: usize) {
    out.clear();
    let l = LINES.lock().unwrap_or_else(|e| e.into_inner());
    let start = l.len().saturating_sub(max);
    for s in l.iter().skip(start) {
        out.push(s.clone());
    }
}

struct LogState {
    last: String,
    dup: u32,
    file: Option<std::fs::File>,
}
static STATE: Mutex<LogState> = Mutex::new(LogState {
    last: String::new(),
    dup: 0,
    file: None,
});

/// Colour a `[tag]` by subsystem (prefix match, so `pawnfx` inherits `pawn`).
fn tag_color(tag: &str) -> &'static str {
    const MAP: &[(&str, &str)] = &[
        ("freecam", "\x1b[38;2;97;175;239m"),
        ("camhook", "\x1b[38;2;97;175;239m"),
        ("cam", "\x1b[38;2;97;175;239m"),
        ("panel", "\x1b[38;2;198;120;221m"),
        ("imgui", "\x1b[38;2;198;120;221m"),
        ("demo", "\x1b[38;2;198;120;221m"),
        ("hud", "\x1b[38;2;198;120;221m"),
        ("fullbody", "\x1b[38;2;152;195;121m"),
        ("scale", "\x1b[38;2;152;195;121m"),
        ("pawn", "\x1b[38;2;229;192;123m"),
        ("cheat", "\x1b[38;2;229;192;123m"),
        ("freeze", "\x1b[38;2;229;192;123m"),
        ("time", "\x1b[38;2;86;182;194m"),
        ("cine", "\x1b[38;2;86;182;194m"),
        ("fault", "\x1b[38;2;224;108;117m"),
        ("err", "\x1b[38;2;224;108;117m"),
        ("console", "\x1b[38;2;128;128;128m"),
        ("hook", "\x1b[38;2;128;128;128m"),
        ("verify", "\x1b[38;2;128;128;128m"),
        ("scan", "\x1b[38;2;128;128;128m"),
    ];
    for (k, c) in MAP {
        if tag.starts_with(k) {
            return c;
        }
    }
    ACCENT
}

fn con_write(s: &str) {
    let h = CONOUT.load(Ordering::Relaxed);
    if h.is_null() {
        return;
    }
    let bytes = s.as_bytes();
    let mut written = 0u32;
    unsafe {
        WriteFile(
            h,
            bytes.as_ptr(),
            bytes.len() as u32,
            &mut written,
            core::ptr::null_mut(),
        );
    }
}

/// Allocate a console, enable UTF-8 + ANSI colour, and title it. We keep a PRIVATE handle
/// to the console for our own output and repoint the process std handles at NUL, so the
/// game's / Steam's own stdout logging (which can include account identifiers) never lands
/// in our console - only our `[tag]` lines appear.
pub fn init_console() {
    unsafe {
        AllocConsole();
        SetConsoleOutputCP(65001); // UTF-8, so glyphs render

        const GENERIC_WRITE: u32 = 0x4000_0000;
        const GENERIC_READ: u32 = 0x8000_0000;
        const FILE_SHARE_RW: u32 = 0x0000_0003;

        let conout: Vec<u16> = "CONOUT$".encode_utf16().chain(core::iter::once(0)).collect();
        let h = CreateFileW(
            conout.as_ptr(),
            GENERIC_WRITE | GENERIC_READ,
            FILE_SHARE_RW,
            core::ptr::null(),
            OPEN_EXISTING,
            0,
            core::ptr::null_mut(),
        );
        CONOUT.store(h as *mut c_void, Ordering::SeqCst);

        // Send the game's stdout/stderr to NUL.
        let nul: Vec<u16> = "NUL".encode_utf16().chain(core::iter::once(0)).collect();
        let devnull = CreateFileW(
            nul.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_RW,
            core::ptr::null(),
            OPEN_EXISTING,
            0,
            core::ptr::null_mut(),
        );
        if devnull != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_OUTPUT_HANDLE, devnull);
            SetStdHandle(STD_ERROR_HANDLE, devnull);
        }

        let mut mode = 0u32;
        if GetConsoleMode(h, &mut mode) != 0 {
            SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
        let title: Vec<u16> = "Huragok - in-game hook engine"
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        SetConsoleTitleW(title.as_ptr());
    }
}

/// Print the startup header.
pub fn banner() {
    let rule = format!("  {DIM}{}{RESET}\n", "\u{2500}".repeat(30));
    con_write(&format!(
        "\n  {BOLD}{ACCENT}\u{25CF}{RESET} {BOLD}Huragok{RESET} {DIM}- in-game hook engine  (build {}){RESET}\n",
        env!("HURAGOK_BUILD")
    ));
    con_write(&rule);
    con_write(&format!(
        "  {DIM}waiting for a mission - hooks automatically once the world loads...{RESET}\n\n"
    ));
}

fn log_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("huragok_log.txt"))
}

/// Emit one log line (backs the [`rep!`](crate::rep) macro).
pub fn emit(msg: &str) {
    let mut st = match STATE.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };

    // Persistent plain-text log next to the exe.
    if st.file.is_none() {
        if let Some(p) = log_path() {
            st.file = std::fs::File::create(p).ok();
        }
    }
    if let Some(f) = st.file.as_mut() {
        let _ = writeln!(f, "{msg}");
        let _ = f.flush();
    }

    // Mirror into the ring buffer the ImGui console reads.
    if let Ok(mut l) = LINES.lock() {
        l.push_back(msg.to_string());
        while l.len() > 200 {
            l.pop_front();
        }
    }

    // Console: collapse consecutive duplicates.
    if msg == st.last {
        st.dup += 1;
        con_write(&format!("\r    {DIM}repeated x{}{RESET}", st.dup + 1));
        return;
    }
    if st.dup > 0 {
        con_write("\n");
        st.dup = 0;
    }
    st.last.clear();
    st.last.push_str(msg);

    // Indented dump sub-item -> dim continuation, no bullet.
    if msg.starts_with(' ') {
        con_write(&format!("      {DIM}{}{RESET}\n", msg.trim_start()));
        return;
    }
    // "[tag] message" -> ● tag  message
    if let Some(rest) = msg.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            let tag = &rest[..end];
            let body = rest[end + 1..].trim_start();
            let c = tag_color(tag);
            con_write(&format!("  {c}\u{25CF}{RESET} {c}{tag:<9}{RESET} {body}\n"));
            return;
        }
    }
    con_write(&format!("  {DIM}\u{2022}{RESET} {msg}\n"));
}

/// Log a line, `println!`-style: `rep!("[tag] {}", value)`.
#[macro_export]
macro_rules! rep {
    ($($arg:tt)*) => {
        $crate::log::emit(&::std::format!($($arg)*))
    };
}
