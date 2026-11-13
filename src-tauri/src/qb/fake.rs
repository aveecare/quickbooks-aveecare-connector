//! A stand-in for QuickBooks in unit tests: scripted answers, a record of
//! every request, and any failure on demand.

use super::{OpenedFile, QbError, QbTransport};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct FakeLog {
    pub opened: u32,
    pub closed: u32,
    pub requests: Vec<String>,
}

pub struct FakeTransport {
    pub company_file: String,
    pub open_error: Option<QbError>,
    /// Answers in order. `Err` makes that one request fail.
    pub answers: VecDeque<Result<String, QbError>>,
    /// Answer for anything not scripted: a HostQuery gets a host, the rest an OK.
    pub log: Arc<Mutex<FakeLog>>,
    is_open: bool,
}

pub const HOST_RESPONSE: &str = r#"<?xml version="1.0" ?><QBXML><QBXMLMsgsRs><HostQueryRs requestID="host" statusCode="0" statusSeverity="Info" statusMessage="Status OK"><HostRet><ProductName>QuickBooks Desktop Pro 2024</ProductName><MajorVersion>34</MajorVersion><MinorVersion>0</MinorVersion><Country>US</Country><SupportedQBXMLVersion>13.0</SupportedQBXMLVersion><SupportedQBXMLVersion>16.0</SupportedQBXMLVersion></HostRet></HostQueryRs></QBXMLMsgsRs></QBXML>"#;

impl FakeTransport {
    pub fn new(company_file: &str) -> Self {
        Self {
            company_file: company_file.into(),
            open_error: None,
            answers: VecDeque::new(),
            log: Arc::new(Mutex::new(FakeLog::default())),
            is_open: false,
        }
    }

    pub fn answer(mut self, a: Result<String, QbError>) -> Self {
        self.answers.push_back(a);
        self
    }

    pub fn failing_open(mut self, e: QbError) -> Self {
        self.open_error = Some(e);
        self
    }
}

pub fn ok_response(rs: &str) -> String {
    format!(r#"<?xml version="1.0" ?><QBXML><QBXMLMsgsRs><{rs} requestID="1" statusCode="0" statusSeverity="Info" statusMessage="Status OK"/></QBXMLMsgsRs></QBXML>"#)
}

impl QbTransport for FakeTransport {
    fn open(&mut self, _app_id: &str, _app_name: &str) -> Result<OpenedFile, QbError> {
        self.log.lock().unwrap().opened += 1;
        if let Some(e) = self.open_error.clone() {
            return Err(e);
        }
        self.is_open = true;
        Ok(OpenedFile { company_file: self.company_file.clone() })
    }

    fn process(&mut self, request: &str) -> Result<String, QbError> {
        assert!(self.is_open, "process() before open()");
        self.log.lock().unwrap().requests.push(request.to_string());
        if request.contains("<HostQueryRq") {
            return Ok(HOST_RESPONSE.to_string());
        }
        match self.answers.pop_front() {
            Some(a) => a,
            None => Ok(ok_response("GenericRs")),
        }
    }

    fn close(&mut self) {
        if self.is_open {
            self.log.lock().unwrap().closed += 1;
        }
        self.is_open = false;
    }
}
