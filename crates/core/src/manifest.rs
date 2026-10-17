//! A small self-describing manifest written to the target root so a machine
//! that has never seen this setup can still understand the layout. This is what
//! makes a drive portable between installs.

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const MANIFEST_NAME: &str = ".timeportal.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Format version of the manifest itself.
    pub schema: u32,
    /// Hostname the backups were taken from.
    pub host: String,
    /// How the target is laid out. Only "btrbk" for now.
    pub layout: String,
    /// Source subvolumes captured, e.g. ["@", "@home"].
    pub subvolumes: Vec<String>,
    /// Extra trees copied alongside (not btrfs subvolumes), e.g. ["boot-esp"].
    #[serde(default)]
    pub includes: Vec<String>,
    /// ISO timestamp of when the manifest was written.
    #[serde(default)]
    pub created: String,
}

impl Manifest {
    pub fn new(host: &str) -> Self {
        Manifest {
            schema: 1,
            host: host.to_string(),
            layout: "btrbk".to_string(),
            subvolumes: vec!["@".to_string(), "@home".to_string()],
            includes: vec!["boot-esp".to_string()],
            created: String::new(),
        }
    }

    pub fn load(dir: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(dir.join(MANIFEST_NAME))?;
        let m = serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(m)
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).unwrap();
        std::fs::write(dir.join(MANIFEST_NAME), text)
    }

    pub fn describe(&self) -> String {
        format!(
            "backups of \"{}\" ({} layout): {} + {}",
            self.host,
            self.layout,
            self.subvolumes.join(", "),
            if self.includes.is_empty() {
                "no extras".to_string()
            } else {
                self.includes.join(", ")
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_a_directory() {
        let d = std::env::temp_dir().join(format!("tmman-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();

        let mut m = Manifest::new("portal");
        m.created = "2026-10-16T12:00:00-07:00".to_string();
        m.save(&d).unwrap();

        let back = Manifest::load(&d).unwrap();
        assert_eq!(m, back);
        assert!(back.describe().contains("portal"));

        std::fs::remove_dir_all(&d).ok();
    }
}
