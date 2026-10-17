//! Snapshot versions as laid out by btrbk on the target: read-only subvolumes
//! named `@.<ts>` (whole root) and `@home.<ts>` (the home subvolume). A "version"
//! is a timestamp for which either or both exist.

use crate::parse_ts;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Version {
    pub ts: String,
    pub epoch: i64,
    pub system: bool,
    pub home: bool,
}

/// Classify a single directory name. Returns (timestamp, is_system, is_home).
pub fn classify(name: &str) -> Option<(String, bool, bool)> {
    if let Some(ts) = name.strip_prefix("@home.") {
        Some((ts.to_string(), false, true))
    } else {
        name.strip_prefix("@.")
            .map(|ts| (ts.to_string(), true, false))
    }
}

/// Scan a mounted target directory for versions, oldest first.
pub fn scan_versions(target: &Path) -> std::io::Result<Vec<Version>> {
    let mut map: BTreeMap<String, (bool, bool)> = BTreeMap::new();

    for entry in std::fs::read_dir(target)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some((ts, sys, home)) = classify(name) {
            // Ignore anything that is not a real timestamp.
            if parse_ts(&ts).is_none() {
                continue;
            }
            let slot = map.entry(ts).or_insert((false, false));
            slot.0 |= sys;
            slot.1 |= home;
        }
    }

    let mut out: Vec<Version> = map
        .into_iter()
        .filter_map(|(ts, (system, home))| {
            parse_ts(&ts).map(|epoch| Version {
                ts,
                epoch,
                system,
                home,
            })
        })
        .collect();
    out.sort_by_key(|v| v.epoch);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tmver-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn classifies_names() {
        assert_eq!(
            classify("@home.20261016T122217-0700"),
            Some(("20261016T122217-0700".into(), false, true))
        );
        assert_eq!(
            classify("@.20261016T122217-0700"),
            Some(("20261016T122217-0700".into(), true, false))
        );
        assert_eq!(classify("boot-esp"), None);
        assert_eq!(classify("@pkg.2026"), None);
    }

    #[test]
    fn scan_pairs_system_and_home_and_sorts() {
        let d = scratch("scan");
        fs::create_dir(d.join("@.20261015T030000-0700")).unwrap();
        fs::create_dir(d.join("@home.20261015T030000-0700")).unwrap();
        fs::create_dir(d.join("@.20261016T120000-0700")).unwrap();
        fs::create_dir(d.join("boot-esp")).unwrap(); // ignored

        let v = scan_versions(&d).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].ts, "20261015T030000-0700");
        assert!(v[0].system && v[0].home);
        // newest second, home-only is possible
        assert_eq!(v[1].ts, "20261016T120000-0700");
        assert!(v[1].system && !v[1].home);

        fs::remove_dir_all(&d).ok();
    }
}
