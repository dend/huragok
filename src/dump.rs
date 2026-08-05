//! `huragok dump` - enumerate the live actor roster from GUObjectArray and write it to
//! `huragok_refs.txt` next to the exe. Character/vehicle/weapon *tag* names for the loaded
//! mission live in pak data we cannot read from disk, but their spawned UClass names (e.g.
//! `BP_GruntBipedActor_C`) are readable here - the closest we can get to the mission roster,
//! and a starting point for the reference-naming problem (mantini characters, object names).

use std::collections::HashMap;

use crate::offsets::{RF_ARCHETYPE_OBJECT, RF_BIT30, RF_CLASS_DEFAULT_OBJECT, UO_FLAGS};
use crate::ue::fname::obj_name;
use crate::ue::object::{num_elements, object_at};
use crate::ue::reflect::{class_of, find_class, is_a};

fn category(name: &str) -> &'static str {
    if name.contains("BipedActor") || name.contains("Biped") {
        "bipeds"
    } else if name.contains("Vehicle") {
        "vehicles"
    } else if name.contains("Weapon") {
        "weapons"
    } else if name.contains("Equipment") || name.contains("Grenade") {
        "equipment"
    } else {
        "other"
    }
}

/// Walk the object table, tally live BlamObjectActor subclasses, write the report. Game thread.
pub fn run(_pc: *mut u8) {
    let boa = find_class("BlamObjectActor");
    if boa.is_null() {
        crate::rep!("[dump] BlamObjectActor class not found - load into a mission first");
        return;
    }

    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut scanned = 0u32;
    crate::seh::guard(|| unsafe {
        let n = num_elements();
        for i in 0..n {
            let o = object_at(i);
            if o.is_null() {
                continue;
            }
            let flags = *((o as usize + UO_FLAGS) as *const u32);
            if flags & (RF_CLASS_DEFAULT_OBJECT | RF_ARCHETYPE_OBJECT | RF_BIT30) != 0 {
                continue;
            }
            let cls = class_of(o);
            if !is_a(cls, boa) {
                continue;
            }
            scanned += 1;
            *counts.entry(obj_name(cls)).or_insert(0) += 1;
        }
    });

    // Group by category, sort each group by descending count.
    let cats = ["bipeds", "vehicles", "weapons", "equipment", "other"];
    let mut report = String::new();
    report.push_str("# Huragok live actor roster (BlamObjectActor subclasses)\n");
    report.push_str("# Spawned UClass names for the currently loaded mission. These are the\n");
    report.push_str("# closest readable equivalent to mantini character / object-name refs.\n\n");
    report.push_str(&format!("total live actors: {scanned}, distinct classes: {}\n\n", counts.len()));

    for cat in cats {
        let mut rows: Vec<(&String, &u32)> =
            counts.iter().filter(|(n, _)| category(n) == cat).collect();
        if rows.is_empty() {
            continue;
        }
        rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        report.push_str(&format!("== {cat} ({}) ==\n", rows.len()));
        for (name, count) in &rows {
            report.push_str(&format!("  {count:4}  {name}\n"));
        }
        report.push('\n');
    }

    // Write next to the exe; echo a short summary + the biped roster (the useful part).
    match crate::log::exe_dir().map(|d| d.join("huragok_refs.txt")) {
        Some(p) => match std::fs::write(&p, &report) {
            Ok(_) => crate::rep!("[dump] roster written: {}", p.display()),
            Err(e) => crate::rep!("[dump] write failed: {e}"),
        },
        None => crate::rep!("[dump] could not resolve exe dir"),
    }

    let mut bipeds: Vec<(&String, &u32)> =
        counts.iter().filter(|(n, _)| category(n) == "bipeds").collect();
    bipeds.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    crate::rep!("[dump] {} live actors, {} classes; biped roster:", scanned, counts.len());
    for (name, count) in bipeds.iter().take(20) {
        crate::rep!("[dump]   {count:4}  {name}");
    }
}
