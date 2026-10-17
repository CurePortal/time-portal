//! Block-device probing, parsed from `lsblk --json`. The GUI/helper use this to
//! find a candidate backup drive (a `crypto_LUKS` volume) and, after unlocking,
//! the btrfs filesystem inside it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockDevice {
    pub name: String,
    pub path: String,
    #[serde(rename = "type")]
    pub dtype: String,
    pub fstype: Option<String>,
    pub label: Option<String>,
    pub uuid: Option<String>,
    pub mountpoint: Option<String>,
    pub size: Option<String>,
    pub children: Vec<BlockDevice>,
}

/// Parse the output of
/// `lsblk -J -o NAME,PATH,TYPE,FSTYPE,LABEL,UUID,MOUNTPOINTS,SIZE`.
pub fn parse_lsblk(json: &str) -> Result<Vec<BlockDevice>, serde_json::Error> {
    let v: Value = serde_json::from_str(json)?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("blockdevices").and_then(Value::as_array) {
        for d in arr {
            out.push(node(d));
        }
    }
    Ok(out)
}

fn node(v: &Value) -> BlockDevice {
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let mountpoint = v
        .get("mountpoints")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find_map(|x| x.as_str()))
        .map(str::to_string);
    let children = v
        .get("children")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(node).collect())
        .unwrap_or_default();
    let name = s("name").unwrap_or_default();
    BlockDevice {
        path: s("path").unwrap_or_else(|| format!("/dev/{name}")),
        name,
        dtype: s("type").unwrap_or_default(),
        fstype: s("fstype"),
        label: s("label"),
        uuid: s("uuid"),
        mountpoint,
        size: s("size"),
        children,
    }
}

impl BlockDevice {
    pub fn flatten<'a>(&'a self, out: &mut Vec<&'a BlockDevice>) {
        out.push(self);
        for c in &self.children {
            c.flatten(out);
        }
    }
}

/// Candidate backup drives: any LUKS container.
pub fn luks_devices(devs: &[BlockDevice]) -> Vec<&BlockDevice> {
    let mut flat = Vec::new();
    for d in devs {
        d.flatten(&mut flat);
    }
    flat.into_iter()
        .filter(|d| d.fstype.as_deref() == Some("crypto_LUKS"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "blockdevices": [
        {"name":"sda","path":"/dev/sda","type":"disk","fstype":null,"label":null,"uuid":null,"mountpoints":[null],"size":"931.5G",
         "children":[
           {"name":"sda1","path":"/dev/sda1","type":"part","fstype":"crypto_LUKS","label":"backup-crypt","uuid":"44afcf5a-8425-4625-9837-72183cc80fb1","mountpoints":[null],"size":"931.5G",
            "children":[
              {"name":"backup","path":"/dev/mapper/backup","type":"crypt","fstype":"btrfs","label":"BACKUP","uuid":"b996caff-85db-45ff-b167-9d894d3ef876","mountpoints":["/mnt/backup"],"size":"931.5G"}
            ]}
         ]},
        {"name":"nvme0n1p2","path":"/dev/nvme0n1p2","type":"part","fstype":"crypto_LUKS","label":null,"uuid":"320554aa","mountpoints":[null],"size":"929.8G","children":[]}
      ]}"#;

    #[test]
    fn finds_luks_devices() {
        let devs = parse_lsblk(SAMPLE).unwrap();
        let luks = luks_devices(&devs);
        assert_eq!(luks.len(), 2);
        assert_eq!(luks[0].name, "sda1");
        assert_eq!(luks[0].label.as_deref(), Some("backup-crypt"));
    }

    #[test]
    fn finds_the_btrfs_inside() {
        let devs = parse_lsblk(SAMPLE).unwrap();
        let luks = luks_devices(&devs);
        let inner = luks[0].children.first().unwrap();
        assert_eq!(inner.fstype.as_deref(), Some("btrfs"));
        assert_eq!(inner.mountpoint.as_deref(), Some("/mnt/backup"));
    }
}
