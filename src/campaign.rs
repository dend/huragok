//! Campaign / mission progress readout, refreshed on the game thread via reflection.
//!
//! - Level name: `UGameplayStatics::GetCurrentLevelName(WorldContext, bRemovePrefix)` -> FString.
//! - Difficulty: `BlamCampaignFlowGameStateComponent.CampaignDifficultyLevel`
//!   (EBlamCampaignDifficultyLevel: 1 Easy, 2 Normal, 3 Heroic, 4 Legendary).
//! Both resolved by name so no fragile hardcoded offsets are needed.

use core::ffi::c_void;
use std::sync::Mutex;

use crate::ue::process_event::process_event;
use crate::ue::reflect::{
    class_of, find_cdo, find_class, find_function, find_live_by_class, property_offset,
};

struct Info {
    level: String,
    difficulty: String,
    checkpoint: i32,
    segment: i32,
}
static INFO: Mutex<Info> = Mutex::new(Info {
    level: String::new(),
    difficulty: String::new(),
    checkpoint: -1,
    segment: -1,
});

/// `(level_name, difficulty, checkpoint, segment)` for the panel (-1 = unknown).
pub fn snapshot() -> (String, String, i32, i32) {
    match INFO.lock() {
        Ok(g) => (g.level.clone(), g.difficulty.clone(), g.checkpoint, g.segment),
        Err(_) => (String::new(), String::new(), -1, -1),
    }
}

/// Map a campaign level code (a10, a50, ...) to its mission title. Falls back to the
/// raw id. Shown as "Truth and Reconciliation (a50)".
fn mission_title(level: &str) -> String {
    let id = level.trim().to_ascii_lowercase();
    let name = if id.contains("a10") {
        "The Pillar of Autumn"
    } else if id.contains("a30") {
        "Halo"
    } else if id.contains("a50") {
        "Truth and Reconciliation"
    } else if id.contains("b30") {
        "The Silent Cartographer"
    } else if id.contains("b40") {
        "Assault on the Control Room"
    } else if id.contains("c10") {
        "343 Guilty Spark"
    } else if id.contains("c20") {
        "The Library"
    } else if id.contains("c40") {
        "Two Betrayals"
    } else if id.contains("d20") {
        "Keyes"
    } else if id.contains("d40") {
        "The Maw"
    } else {
        return level.trim().to_string();
    };
    format!("{} ({})", name, level.trim())
}

/// Read an FString (`TArray<TCHAR>` = {data ptr, i32 num, i32 max}) from a parms buffer.
unsafe fn read_fstring(buf: &[u8], off: usize) -> String {
    if off + 12 > buf.len() {
        return String::new();
    }
    let data = *(buf.as_ptr().add(off) as *const *const u16);
    let num = *(buf.as_ptr().add(off + 8) as *const i32);
    if data.is_null() || num <= 1 || num > 512 {
        return String::new();
    }
    let slice = core::slice::from_raw_parts(data, (num - 1) as usize); // num counts the NUL
    String::from_utf16_lossy(slice)
}

/// Refresh the campaign readout. Call on the game thread (throttled). SEH-guarded.
pub fn refresh(pc: *mut u8) {
    let mut level = String::new();
    let mut difficulty = String::new();
    crate::seh::guard(|| unsafe {
        let gs = find_class("GameplayStatics");
        let cdo = find_cdo("Default__GameplayStatics");
        if !gs.is_null() && !cdo.is_null() {
            let f = find_function(gs, "GetCurrentLevelName");
            if !f.is_null() {
                let mut buf = [0u8; 64];
                *(buf.as_mut_ptr() as *mut *mut u8) = pc; // WorldContextObject @ 0
                buf[8] = 1; // bRemovePrefixString @ 8
                process_event(cdo, f, buf.as_mut_ptr() as *mut c_void);
                level = read_fstring(&buf, 0x10); // FString return after the two args
            }
        }

        let comp = find_live_by_class("BlamCampaignFlowGameStateComponent");
        if !comp.is_null() {
            if let Some(off) = property_offset(class_of(comp), "CampaignDifficultyLevel") {
                if (0..0x4000).contains(&off) {
                    let v = *((comp as usize + off as usize) as *const u8);
                    difficulty = match v {
                        1 => "Easy",
                        2 => "Normal",
                        3 => "Heroic",
                        4 => "Legendary",
                        _ => "",
                    }
                    .to_string();
                }
            }
        }
    });

    // Segment = current zone-set index (static DLL global, game-thread safe).
    // Checkpoint = insertion-point index from the sim game-globals block.
    let mut checkpoint = -1i32;
    let mut segment = -1i32;
    crate::seh::guard(|| unsafe {
        let sb = crate::mem::sim_base();
        if sb != 0 {
            segment = *((sb + 0x009a_14e0) as *const i32);
        }
        let blk = crate::simtime::game_globals();
        if blk != 0 {
            checkpoint = *((blk + 0x1f0) as *const u16) as i32;
        }
    });

    if let Ok(mut g) = INFO.lock() {
        if !level.is_empty() {
            g.level = mission_title(&level);
        }
        g.difficulty = difficulty;
        g.checkpoint = checkpoint;
        g.segment = segment;
    }
}

/// Mission-name probe: dump the campaign-flow reflection so we can wire real mission titles
/// (including the extra missions the hardcoded map misses). Logs what's reachable.
pub fn diag_mission(pc: *mut u8) {
    use crate::ue::fname::obj_name;
    crate::seh::guard(|| unsafe {
        let gs = find_class("GameplayStatics");
        let cdo = find_cdo("Default__GameplayStatics");
        if !gs.is_null() && !cdo.is_null() {
            let f = find_function(gs, "GetCurrentLevelName");
            if !f.is_null() {
                let mut buf = [0u8; 64];
                *(buf.as_mut_ptr() as *mut *mut u8) = pc;
                buf[8] = 1;
                process_event(cdo, f, buf.as_mut_ptr() as *mut c_void);
                crate::rep!("[mission] GetCurrentLevelName = '{}'", read_fstring(&buf, 0x10));
            }
        }
        let comp = find_live_by_class("BlamCampaignFlowGameStateComponent");
        if comp.is_null() {
            crate::rep!("[mission] BlamCampaignFlowGameStateComponent not live");
            return;
        }
        crate::rep!("[mission] flow comp @ {:p} class={}", comp, obj_name(class_of(comp)));
        // ActiveCampaign object pointer.
        if let Some(off) = property_offset(class_of(comp), "ActiveCampaign") {
            let ac = *((comp as usize + off as usize) as *const *mut u8);
            crate::rep!("[mission] ActiveCampaign +0x{:x} = {:p}", off, ac);
            if !ac.is_null() {
                crate::rep!("[mission] campaign class={}", obj_name(class_of(ac)));
                // ScenarioList TArray {data ptr, num, max}.
                if let Some(so) = property_offset(class_of(ac), "ScenarioList") {
                    let data = *((ac as usize + so as usize) as *const usize);
                    let num = *((ac as usize + so as usize + 8) as *const i32);
                    crate::rep!("[mission] ScenarioList +0x{:x} data=0x{:x} num={}", so, data, num);
                }
            }
        }
        // StartingScenarioName / difficulty property, for reference.
        for pn in ["StartingScenarioName", "CampaignDifficultyLevel", "CurrentScenarioIndex"] {
            if let Some(o) = property_offset(class_of(comp), pn) {
                crate::rep!("[mission] prop {} @ +0x{:x}", pn, o);
            }
        }
    });
}
