//! What the app remembers between launches: settings and the activity list.
//! Plain JSON under %LOCALAPPDATA%\AveeCare QuickBooks Connector. The sign-in
//! itself is not here; it lives in Windows Credential Manager (session.rs).

use crate::config::DEFAULT_SERVER;
use crate::sync::RunRecord;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How many runs the activity list keeps.
pub const HISTORY_LIMIT: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub server_url: String,
    /// Minutes between automatic syncs; 0 is off.
    pub auto_sync_minutes: u32,
    pub start_with_windows: bool,
    /// The email last used to sign in, to prefill the form.
    pub last_email: String,
    /// The company file the last good sync used, per agency, so a different
    /// open file is caught before anything is sent to it.
    pub known_files: Vec<KnownFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KnownFile {
    pub company_name: String,
    pub company_file: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self { server_url: DEFAULT_SERVER.into(), auto_sync_minutes: 0, start_with_windows: false, last_email: String::new(), known_files: vec![] }
    }
}

impl Settings {
    pub fn known_file(&self, company: &str) -> Option<&str> {
        self.known_files.iter().find(|k| k.company_name == company).map(|k| k.company_file.as_str())
    }

    pub fn remember_file(&mut self, company: &str, file: &str) {
        if company.is_empty() || file.is_empty() {
            return;
        }
        self.known_files.retain(|k| k.company_name != company);
        self.known_files.push(KnownFile { company_name: company.into(), company_file: file.into() });
    }
}

/// Same file? QuickBooks reports the full path; compare case-insensitively,
/// as Windows does.
pub fn same_file(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }

    /// `AVEECARE_QBC_DATA_DIR` overrides it for testing, so a test run never
    /// reads or writes a real install's settings and activity.
    pub fn default_dir() -> PathBuf {
        if let Some(d) = std::env::var_os("AVEECARE_QBC_DATA_DIR").filter(|d| !d.is_empty()) {
            return PathBuf::from(d);
        }
        dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("AveeCare QuickBooks Connector")
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn read<T: for<'de> Deserialize<'de> + Default>(&self, name: &str) -> T {
        std::fs::read_to_string(self.dir.join(name)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// Write via a temporary file so a crash mid-write never leaves half a file.
    fn write<T: Serialize>(&self, name: &str, value: &T) -> std::io::Result<()> {
        let tmp = self.dir.join(format!("{name}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
        std::fs::rename(&tmp, self.dir.join(name))
    }

    pub fn settings(&self) -> Settings {
        let mut s: Settings = self.read("settings.json");
        if s.server_url.trim().is_empty() {
            s.server_url = DEFAULT_SERVER.into();
        }
        s
    }

    pub fn save_settings(&self, s: &Settings) -> std::io::Result<()> {
        self.write("settings.json", s)
    }

    /// Newest first.
    pub fn history(&self) -> Vec<RunRecord> {
        self.read("history.json")
    }

    pub fn add_run(&self, run: &RunRecord) -> std::io::Result<Vec<RunRecord>> {
        let mut h = self.history();
        h.retain(|r| r.id != run.id);
        h.insert(0, run.clone());
        h.truncate(HISTORY_LIMIT);
        self.write("history.json", &h)?;
        Ok(h)
    }

    pub fn clear_history(&self) -> std::io::Result<()> {
        self.write::<Vec<RunRecord>>("history.json", &vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{RunStatus, RunTotals};

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("aveecare-qbc-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn rec(id: &str) -> RunRecord {
        RunRecord {
            id: id.into(),
            started_at: "2026-11-12T10:00:00Z".into(),
            finished_at: "2026-11-12T10:01:00Z".into(),
            trigger: "manual".into(),
            status: RunStatus::Completed,
            mode: "timesheet".into(),
            plan: Default::default(),
            company_file: String::new(),
            product_name: String::new(),
            qbxml_version: String::new(),
            steps: vec![],
            totals: RunTotals::default(),
            problem: None,
            closing: String::new(),
            requests: 0,
        }
    }

    #[test]
    fn history_is_newest_first_and_bounded() {
        let d = tmp();
        let s = Store::new(d.clone());
        for i in 0..(HISTORY_LIMIT + 5) {
            s.add_run(&rec(&i.to_string())).unwrap();
        }
        let h = s.history();
        assert_eq!(h.len(), HISTORY_LIMIT);
        assert_eq!(h[0].id, (HISTORY_LIMIT + 4).to_string());
        s.clear_history().unwrap();
        assert!(s.history().is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn settings_round_trip_and_default_server() {
        let d = tmp();
        let s = Store::new(d.clone());
        assert_eq!(s.settings().server_url, DEFAULT_SERVER);
        let mut st = s.settings();
        st.auto_sync_minutes = 30;
        st.remember_file("Angels", "C:\\Q\\Angels.QBW");
        st.remember_file("Angels", "C:\\Q\\Angels2.QBW");
        s.save_settings(&st).unwrap();
        let back = s.settings();
        assert_eq!(back.auto_sync_minutes, 30);
        assert_eq!(back.known_files.len(), 1);
        assert_eq!(back.known_file("Angels"), Some("C:\\Q\\Angels2.QBW"));
        assert!(same_file("c:\\q\\angels2.qbw", "C:\\Q\\Angels2.QBW"));
        std::fs::remove_dir_all(d).unwrap();
    }
}
