//! The QuickBooks emulator as the far end, for the end-to-end test only.
//!
//! The emulator models a QuickBooks Desktop company file and answers
//! `POST /qbxml` with a qbXML response. This transport is honored only for a
//! loopback address, so no setting can send QuickBooks traffic off the machine.

use super::{OpenedFile, QbError, QbErrorKind, QbTransport};
use crate::config::USER_AGENT;
use std::time::Duration;

pub struct EmulatorTransport {
    url: String,
    http: reqwest::blocking::Client,
}

/// Only http://localhost, 127.0.0.1 or [::1].
pub fn loopback_url(raw: &str) -> Option<String> {
    let u = url::Url::parse(raw.trim()).ok()?;
    if u.scheme() != "http" {
        return None;
    }
    let ok = match u.host()? {
        url::Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    };
    ok.then(|| u.as_str().trim_end_matches('/').to_string())
}

/// Whether the emulator answers right now (the status card's "is QuickBooks
/// open" for the emulator). A refused loopback connection fails at once.
pub fn answering(url: &str) -> bool {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_millis(1500))
        .build()
        .ok()
        .and_then(|c| c.get(format!("{url}/file")).send().ok())
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

impl EmulatorTransport {
    pub fn new(raw_url: &str) -> Result<Self, QbError> {
        let url = loopback_url(raw_url).ok_or_else(|| {
            QbError::new(QbErrorKind::Other, "The QuickBooks emulator address must be on this computer.", "Use http://127.0.0.1:<port>.")
        })?;
        let http = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(60))
            .build()
            .expect("http client");
        Ok(Self { url, http })
    }

    fn lost(e: impl std::fmt::Display) -> QbError {
        let mut err = QbError::new(QbErrorKind::NotRunning, "The QuickBooks emulator is not answering.", "Start the QuickBooks emulator.");
        err.raw = e.to_string();
        err
    }
}

impl QbTransport for EmulatorTransport {
    fn open(&mut self, _app_id: &str, _app_name: &str) -> Result<OpenedFile, QbError> {
        let v: serde_json::Value = self
            .http
            .get(format!("{}/file", self.url))
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.json())
            .map_err(Self::lost)?;
        Ok(OpenedFile {
            company_file: v.get("companyFile").and_then(|c| c.as_str()).unwrap_or("Emulated company file.QBW").to_string(),
        })
    }

    fn process(&mut self, request: &str) -> Result<String, QbError> {
        let res = self
            .http
            .post(format!("{}/qbxml", self.url))
            .header("Content-Type", "text/xml; charset=utf-8")
            .body(request.to_string())
            .send()
            .map_err(Self::lost)?;
        let status = res.status().as_u16();
        let body = res.text().map_err(Self::lost)?;
        if status == 200 {
            return Ok(body);
        }
        // The shim answers 422 with an HRESULT when the emulator, like
        // QuickBooks, could not read the request at all.
        if let Some(h) = body.strip_prefix("HRESULT ") {
            let mut parts = h.splitn(2, ' ');
            let code = parts.next().and_then(|c| u32::from_str_radix(c.trim_start_matches("0x"), 16).ok()).unwrap_or(0x80040400);
            return Err(super::classify_hresult(code, parts.next().unwrap_or("")));
        }
        Err(Self::lost(format!("HTTP {status}: {body}")))
    }

    fn close(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_is_accepted() {
        assert_eq!(loopback_url("http://127.0.0.1:4791/").as_deref(), Some("http://127.0.0.1:4791"));
        assert!(loopback_url("http://localhost:4791").is_some());
        assert!(loopback_url("http://[::1]:4791").is_some());
        assert!(loopback_url("http://127.1.2.3:4791").is_some());
        assert!(loopback_url("http://example.com:4791").is_none());
        assert!(loopback_url("https://127.0.0.1:4791").is_none());
        assert!(loopback_url("http://10.0.0.5").is_none());
        assert!(loopback_url("not a url").is_none());
    }
}
