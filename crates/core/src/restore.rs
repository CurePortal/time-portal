//! Restore planning. Pure path/structure logic; executing a plan is the
//! helper's job. Keeping the "what goes where" here means it can be unit-tested
//! without touching a disk.

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RestoreStep {
    pub what: String,
    pub source: PathBuf,
    pub dest: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RestorePlan {
    pub version_ts: String,
    pub steps: Vec<RestoreStep>,
}

impl RestorePlan {
    /// Human summary, one line per step.
    pub fn summary(&self) -> Vec<String> {
        self.steps
            .iter()
            .map(|s| format!("{}: {} -> {}", s.what, s.source.display(), s.dest.display()))
            .collect()
    }
}

/// Restore one user's home out of a version into `dest` (e.g. `$HOME`).
pub fn plan_home(target_root: &Path, ts: &str, source_user: &str, dest: &Path) -> RestorePlan {
    RestorePlan {
        version_ts: ts.to_string(),
        steps: vec![RestoreStep {
            what: format!("home of {source_user}"),
            source: target_root.join(format!("@home.{ts}")).join(source_user),
            dest: dest.to_path_buf(),
        }],
    }
}

/// Restore the whole system tree of a version onto a mounted root (the bare-
/// metal / live-USB case).
pub fn plan_system(target_root: &Path, ts: &str, mounted_root: &Path) -> RestorePlan {
    RestorePlan {
        version_ts: ts.to_string(),
        steps: vec![RestoreStep {
            what: "system".to_string(),
            source: target_root.join(format!("@.{ts}")),
            dest: mounted_root.to_path_buf(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_plan_points_at_the_user_inside_the_version() {
        let p = plan_home(
            Path::new("/mnt/backup/portal"),
            "20261016T122217-0700",
            "cure",
            Path::new("/home/newuser"),
        );
        assert_eq!(p.steps.len(), 1);
        assert_eq!(
            p.steps[0].source,
            PathBuf::from("/mnt/backup/portal/@home.20261016T122217-0700/cure")
        );
        assert_eq!(p.steps[0].dest, PathBuf::from("/home/newuser"));
    }

    #[test]
    fn system_plan_targets_the_root() {
        let p = plan_system(
            Path::new("/mnt/backup/portal"),
            "20261016T122217-0700",
            Path::new("/mnt/target"),
        );
        assert_eq!(
            p.steps[0].source,
            PathBuf::from("/mnt/backup/portal/@.20261016T122217-0700")
        );
        assert!(p.summary()[0].contains("system"));
    }
}
