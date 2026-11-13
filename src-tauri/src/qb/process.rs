//! Is QuickBooks running, and is it running as administrator?
//!
//! QuickBooks Desktop's COM server refuses a caller at a lower integrity level
//! than QuickBooks itself, and the refusal reads "Could not start QuickBooks",
//! which sounds like QuickBooks is closed. Looking at the process first lets
//! the app say what is really wrong.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};

/// QuickBooks Desktop's main executable: QBW32.EXE through 2022, QBW.EXE (64-bit) from 2022 on.
const QB_EXES: [&str; 2] = ["qbw32.exe", "qbw.exe"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QbProcess {
    NotRunning,
    /// Running; `Some(true)` when it is elevated, `None` when we cannot tell.
    Running { elevated: Option<bool> },
}

fn token_elevated(process: HANDLE) -> Option<bool> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let r = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        let _ = CloseHandle(token);
        r.ok()?;
        Some(elevation.TokenIsElevated != 0)
    }
}

/// Is this app running as administrator?
pub fn self_elevated() -> bool {
    token_elevated(unsafe { GetCurrentProcess() }).unwrap_or(false)
}

pub fn quickbooks_process() -> QbProcess {
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return QbProcess::Running { elevated: None };
        };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found: Option<u32> = None;
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_ascii_lowercase();
                if QB_EXES.contains(&name.as_str()) {
                    found = Some(entry.th32ProcessID);
                    break;
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        let Some(pid) = found else { return QbProcess::NotRunning };
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => {
                let elevated = token_elevated(h);
                let _ = CloseHandle(h);
                QbProcess::Running { elevated }
            }
            // A non-elevated process may not open an elevated one's token.
            Err(_) => QbProcess::Running { elevated: if self_elevated() { None } else { Some(true) } },
        }
    }
}
