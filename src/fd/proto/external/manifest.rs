//! Module manifests: `<dir>/<module>/module.json` says which stock `<comms>` tags an installed module serves and how to start it:
//! `{"serves":["bacnet-server","bacnet-client"],"command":"python3 module.py"}`. The command runs in the module's directory.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Deserialize)]
struct Manifest {
    serves: Vec<String>,
    command: String,
}

/// tag -> (module directory, command)
type Served = HashMap<String, (PathBuf, String)>;

static DIR: OnceLock<PathBuf> = OnceLock::new();
static SERVED: OnceLock<Served> = OnceLock::new();

/// Where modules are installed, from `--modules-dir`; set once, before the first lookup.
pub fn set_dir(p: PathBuf) {
    let _ = DIR.set(p);
}

/// `--modules-dir`, else `$FERROFIRE_MODULES`, else `/opt/ferrofire/modules`.
pub fn dir() -> PathBuf {
    DIR.get().cloned().unwrap_or_else(|| {
        std::env::var_os("FERROFIRE_MODULES").map_or("/opt/ferrofire/modules".into(), Into::into)
    })
}

/// The module directory and command of the installed module that serves `tag`; the directory is scanned once.
pub fn lookup(tag: &str) -> Option<(PathBuf, String)> {
    if cfg!(test) {
        // tests set the directory after others have looked, so no cache
        return scan(&dir()).remove(tag);
    }
    SERVED.get_or_init(|| scan(&dir())).get(tag).cloned()
}

/// Read every `<dir>/*/module.json`; a missing directory is no modules, a bad manifest is a warning naming the file. Entries are
/// visited in name order, and the first module to serve a tag keeps it.
fn scan(dir: &Path) -> Served {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("module.json"))
        .filter(|p| p.is_file())
        .collect();
    paths.sort();
    let mut served = Served::new();
    for p in paths {
        let read = std::fs::read(&p)
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice::<Manifest>(&b).map_err(|e| e.to_string()));
        match read {
            Ok(m) => {
                let home = p.parent().unwrap_or(dir).to_path_buf();
                for tag in m.serves {
                    served
                        .entry(tag)
                        .or_insert_with(|| (home.clone(), m.command.clone()));
                }
            }
            Err(e) => tracing::warn!("module manifest {}: {e}", p.display()),
        }
    }
    served
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A fresh temp directory holding one manifest per `(module, json)`.
    pub fn install(name: &str, manifests: &[(&str, &str)]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ff-manifest-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (m, json) in manifests {
            std::fs::create_dir_all(d.join(m)).unwrap();
            std::fs::write(d.join(m).join("module.json"), json).unwrap();
        }
        d
    }

    /// Point the process at a directory whose manifests serve the 104 tags and `fake-server`.
    pub fn global() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let m = r#"{"serves":["iec60870-5-104-server","fake-server"],"command":"sh m.sh"}"#;
            set_dir(install("global", &[("iec", m)]));
        });
    }

    #[test]
    fn lookup_finds_a_served_tag() {
        let d = install(
            "two",
            &[
                (
                    "a",
                    r#"{"serves":["iec60870-5-104-server","iec60870-5-104-client"],"command":"python3 module.py"}"#,
                ),
                ("b", r#"{"serves":["bacnet-server"],"command":"b.sh -v"}"#),
            ],
        );
        let s = scan(&d);
        assert_eq!(
            s["iec60870-5-104-client"],
            (d.join("a"), "python3 module.py".to_string())
        );
        assert_eq!(s["bacnet-server"], (d.join("b"), "b.sh -v".to_string()));
        assert!(!s.contains_key("modbus-server"));
        assert!(scan(&d.join("missing")).is_empty());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn bad_manifest_is_a_warning_not_an_error() {
        let d = install(
            "bad",
            &[
                ("a", "not json"),
                ("b", r#"{"serves":["x"]}"#),
                ("c", r#"{"serves":["ok"],"command":"c"}"#),
            ],
        );
        std::fs::create_dir_all(d.join("empty")).unwrap();
        let s = scan(&d);
        assert_eq!(s.keys().collect::<Vec<_>>(), ["ok"]);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
