//! Talking to QuickBooks Desktop on this machine.
//!
//! The real transport is Intuit's own COM interface (`QBXMLRP2.RequestProcessor`)
//! in [`com`]. It sits behind [`QbTransport`] so the sync loop is tested
//! against [`fake::FakeTransport`], and so the end-to-end test can stand the
//! QuickBooks emulator in for QuickBooks ([`emulator`]).

pub mod emulator;
pub mod fake;
pub mod host;

#[cfg(windows)]
pub mod com;
#[cfg(windows)]
pub mod process;

use serde::Serialize;

/// What went wrong, in the terms the person can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum QbErrorKind {
    /// QuickBooks (or its SDK component) is not installed on this computer.
    NotInstalled,
    /// QuickBooks is not running.
    NotRunning,
    /// QuickBooks is running but no company file is open.
    NoCompanyOpen,
    /// QuickBooks is running as administrator and the Connector is not (or the reverse).
    ElevationMismatch,
    /// QuickBooks has not been told to let the Connector in (or it was refused).
    NotAuthorized,
    /// A window is open inside QuickBooks that must be closed first.
    Busy,
    /// Another application holds the file in a mode we cannot share.
    InUse,
    /// QuickBooks could not read one request we sent. Affects that request only.
    BadRequest,
    /// QuickBooks stopped answering partway.
    ConnectionLost,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QbError {
    pub kind: QbErrorKind,
    /// "0x80040408" when QuickBooks gave one.
    pub hresult: Option<String>,
    /// One sentence: what happened.
    pub title: String,
    /// What to do about it.
    pub fix: String,
    /// QuickBooks' own words, for support.
    pub raw: String,
}

impl QbError {
    pub fn new(kind: QbErrorKind, title: &str, fix: &str) -> Self {
        Self { kind, hresult: None, title: title.into(), fix: fix.into(), raw: String::new() }
    }

    /// A failure that belongs to one request rather than the connection, so
    /// the run carries on and the server records it against that line.
    pub fn is_line_level(&self) -> bool {
        self.kind == QbErrorKind::BadRequest
    }
}

impl std::fmt::Display for QbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.title)
    }
}

/// The open company file, as QuickBooks reports it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedFile {
    /// Full path of the company file QuickBooks has open.
    pub company_file: String,
}

/// One conversation with QuickBooks: open, any number of requests, close.
pub trait QbTransport {
    /// Attach to the company file open in QuickBooks. Never starts QuickBooks.
    fn open(&mut self, app_id: &str, app_name: &str) -> Result<OpenedFile, QbError>;
    /// One qbXML request, one qbXML response.
    fn process(&mut self, request: &str) -> Result<String, QbError>;
    /// End the session and release QuickBooks. Safe to call more than once.
    fn close(&mut self);
}

/// QBXMLRP2's HRESULTs, in words. Codes from Intuit's QuickBooks SDK
/// Programmer's Guide (Request Processor errors) and the Web Connector guide's
/// appendix A; the elevation case is this project's own measurement.
pub fn classify_hresult(hresult: u32, raw: &str) -> QbError {
    use QbErrorKind::*;
    let (kind, title, fix) = match hresult {
        0x80040154 | 0x800401F3 => (
            NotInstalled,
            "QuickBooks Desktop is not installed on this computer.",
            "Install the Connector on the computer where QuickBooks Desktop is installed.",
        ),
        0x80040400 => (
            BadRequest,
            "QuickBooks could not read one request.",
            "The rest of the sync carried on. AveeCare has been told which item it was.",
        ),
        0x80040408 | 0x80040401 => (
            ElevationMismatch,
            "QuickBooks did not let the Connector connect.",
            "If QuickBooks is open, it may be running as administrator. Close QuickBooks and open it normally (not \"Run as administrator\"), then sync again.",
        ),
        0x80040416 | 0x80040417 | 0x80040403 => (
            NoCompanyOpen,
            "QuickBooks has no company file open.",
            "Open your company file in QuickBooks, then sync again.",
        ),
        0x8004040A => (
            NoCompanyOpen,
            "QuickBooks has a different company file open.",
            "Open the company file your agency uses for AveeCare, then sync again.",
        ),
        0x80040418 => (
            NotAuthorized,
            "QuickBooks has not given the Connector permission yet.",
            "Open QuickBooks as the Admin user, with your company file open, and sync again. QuickBooks will ask whether to allow \"AveeCare Connector\": choose \"Yes, always; allow access even if QuickBooks is not running\" and Continue.",
        ),
        0x8004041A | 0x8004041D => (
            NotAuthorized,
            "QuickBooks does not allow the Connector into this company file.",
            "In QuickBooks, sign in as Admin, open Edit > Preferences > Integrated Applications > Company Preferences, select \"AveeCare Connector\", allow access, and sync again.",
        ),
        0x80040420 => (
            NotAuthorized,
            "Access was declined in QuickBooks.",
            "Sync again and choose \"Yes, always\" when QuickBooks asks about \"AveeCare Connector\". If it does not ask, allow it under Edit > Preferences > Integrated Applications.",
        ),
        0x80040414 => (
            Busy,
            "A window is open inside QuickBooks.",
            "Close any open windows or messages in QuickBooks, then sync again.",
        ),
        0x80040424 => (
            Busy,
            "QuickBooks is still starting.",
            "Wait for QuickBooks to finish opening your company file, then sync again.",
        ),
        0x80040410 | 0x80040422 => (
            InUse,
            "QuickBooks has the company file open in a way the Connector cannot share.",
            "Close other programs that are connected to QuickBooks, or switch QuickBooks to multi-user mode, then sync again.",
        ),
        0x8004040D | 0x80040402 | 0x800706BA | 0x800706BE | 0x80010108 => (
            ConnectionLost,
            "QuickBooks stopped answering.",
            "Make sure QuickBooks is still open, then sync again. Anything that already landed is safe and will not be sent twice.",
        ),
        _ => (Other, "QuickBooks reported a problem.", "Make sure QuickBooks is open with your company file, then sync again."),
    };
    QbError { kind, hresult: Some(format!("0x{hresult:08X}")), title: title.into(), fix: fix.into(), raw: raw.trim().into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hresults_read_as_actions() {
        assert_eq!(classify_hresult(0x80040416, "").kind, QbErrorKind::NoCompanyOpen);
        assert_eq!(classify_hresult(0x80040408, "Could not start QuickBooks.").kind, QbErrorKind::ElevationMismatch);
        assert_eq!(classify_hresult(0x8004041A, "").kind, QbErrorKind::NotAuthorized);
        assert_eq!(classify_hresult(0x80040420, "").kind, QbErrorKind::NotAuthorized);
        assert_eq!(classify_hresult(0x80040414, "").kind, QbErrorKind::Busy);
        assert_eq!(classify_hresult(0x80040154, "").kind, QbErrorKind::NotInstalled);
        let e = classify_hresult(0x80040400, "QuickBooks found an error when parsing the provided XML text stream.");
        assert!(e.is_line_level());
        assert_eq!(e.hresult.as_deref(), Some("0x80040400"));
        assert!(!classify_hresult(0x80040408, "").is_line_level());
        assert_eq!(classify_hresult(0x12345678, "odd").kind, QbErrorKind::Other);
    }
}
