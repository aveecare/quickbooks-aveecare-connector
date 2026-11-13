//! The AveeCare side: `/api/qbwc/connector`.
//!
//! The app carries qbXML and nothing else. Every pay line, invoice and payment
//! is built, decided and recorded on the server by the same session machinery
//! Intuit's Web Connector uses; these calls are that conversation's verbs.

use crate::auth::CognitoConfig;
use crate::config::{APP_VERSION, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone, thiserror::Error, Serialize, PartialEq)]
#[serde(tag = "code", content = "message")]
pub enum ApiError {
    /// The sign-in is missing or no longer accepted.
    #[error("{0}")]
    SignedOut(String),
    /// AveeCare refused: not allowed, QuickBooks Desktop not turned on, and so on.
    #[error("{message}")]
    Refused { reason: String, message: String },
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    Server(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuickBooksDesktopState {
    pub product_name: Option<String>,
    pub last_run_at: Option<String>,
    pub last_run_status: Option<String>,
    pub connected_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    #[serde(default)]
    pub first_name: String,
    #[serde(default)]
    pub last_name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub company_name: String,
    #[serde(default)]
    pub quick_books_desktop: QuickBooksDesktopState,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    #[serde(default)]
    pub timesheet_lines: u32,
    #[serde(default)]
    pub visits: u32,
    #[serde(default)]
    pub invoices: u32,
    #[serde(default)]
    pub payments: u32,
    /// Undo and update-in-place changes this run carries.
    #[serde(default)]
    pub changes: u32,
    /// Mileage reimbursement checks this run writes.
    #[serde(default)]
    pub mileage_checks: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OpenedSession {
    pub ticket: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub plan: Plan,
}

/// What one step did, described by the server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StepResult {
    pub kind: String,
    pub outcome: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minutes: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub txn_id: Option<String>,
    #[serde(default)]
    pub status_code: i64,
}

/// What closing a session returns: the server's closing sentence, plus any
/// results no step answer carried (undo/update records decided from the read
/// alone: held for the office, already matching, already gone).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Closed {
    pub message: String,
    pub results: Vec<StepResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StepAnswer {
    pub percent: i32,
    pub result: Option<StepResult>,
}

#[derive(Debug, Clone, Default)]
pub struct OpenRequest {
    pub company_file: String,
    pub product_name: String,
}

#[derive(Debug, Clone, Default)]
pub struct NextRequest {
    pub hcp_response: String,
    pub company_file: String,
    pub country: String,
    pub major: String,
    pub minor: String,
}

/// The server's verbs. A trait so the sync loop is tested against a fake.
pub trait ConnectorApi: Send + Sync {
    fn profile(&self, token: &str) -> Result<Profile, ApiError>;
    fn authenticate(&self, token: &str, req: &OpenRequest) -> Result<OpenedSession, ApiError>;
    fn send_request_xml(&self, token: &str, ticket: &str, req: &NextRequest) -> Result<String, ApiError>;
    fn receive_response_xml(&self, token: &str, ticket: &str, response: &str, hresult: &str, message: &str) -> Result<StepAnswer, ApiError>;
    fn get_last_error(&self, token: &str, ticket: &str) -> Result<String, ApiError>;
    fn connection_error(&self, token: &str, ticket: &str, hresult: &str, message: &str) -> Result<(), ApiError>;
    fn close_connection(&self, token: &str, ticket: &str) -> Result<Closed, ApiError>;
}

/// What the server says about itself before anyone signs in.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    #[serde(default)]
    pub protocol: u32,
    pub cognito: Option<CognitoConfig>,
}

pub struct HttpConnectorApi {
    base: String,
    http: reqwest::blocking::Client,
}

impl HttpConnectorApi {
    pub fn new(server_url: &str) -> Self {
        let http = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(120))
            .build()
            .expect("http client");
        Self { base: server_url.trim_end_matches('/').to_string(), http }
    }

    fn url(&self) -> String {
        format!("{}/api/qbwc/connector", self.base)
    }

    pub fn server_info(&self) -> Result<ServerInfo, ApiError> {
        let res = self.http.get(self.url()).send().map_err(|e| ApiError::Network(network_sentence(&e)))?;
        if !res.status().is_success() {
            return Err(ApiError::Server(format!(
                "AveeCare answered {} at {}. Check the server address.",
                res.status().as_u16(),
                self.base
            )));
        }
        res.json::<ServerInfo>().map_err(|_| ApiError::Server("AveeCare's answer could not be read. Check the server address.".into()))
    }

    fn post(&self, token: &str, body: Value) -> Result<Value, ApiError> {
        let res = self
            .http
            .post(self.url())
            .bearer_auth(token)
            .json(&body)
            .send()
            .map_err(|e| ApiError::Network(network_sentence(&e)))?;
        let status = res.status().as_u16();
        let v: Value = res.json().unwrap_or(Value::Null);
        let ok = v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);
        if (200..300).contains(&status) && ok {
            return Ok(v);
        }
        let reason = v.get("error").and_then(|e| e.as_str()).unwrap_or("").to_string();
        let message = v.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
        Err(api_error(status, &reason, &message))
    }
}

/// A refusal in words, preferring the server's own sentence.
pub fn api_error(status: u16, reason: &str, message: &str) -> ApiError {
    if status == 401 || reason == "SIGNED_OUT" {
        return ApiError::SignedOut(if message.is_empty() { "Your AveeCare sign-in has ended. Sign in again.".into() } else { message.into() });
    }
    if reason == "UNKNOWN_TICKET" {
        return ApiError::Refused {
            reason: reason.into(),
            message: "AveeCare no longer recognizes this sync. Start it again.".into(),
        };
    }
    if status == 403 || !message.is_empty() {
        return ApiError::Refused {
            reason: reason.into(),
            message: if message.is_empty() { "AveeCare refused this request.".into() } else { message.into() },
        };
    }
    if status >= 500 {
        return ApiError::Server("AveeCare had a problem. Try again in a minute.".into());
    }
    ApiError::Server(format!("AveeCare answered {status} ({reason})."))
}

fn network_sentence(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "AveeCare did not answer in time. Check your internet connection.".into()
    } else {
        "Could not reach AveeCare. Check your internet connection.".into()
    }
}

impl ConnectorApi for HttpConnectorApi {
    fn profile(&self, token: &str) -> Result<Profile, ApiError> {
        let v = self.post(token, json!({ "action": "profile" }))?;
        serde_json::from_value(v.get("profile").cloned().unwrap_or(Value::Null))
            .map_err(|_| ApiError::Server("AveeCare's answer could not be read.".into()))
    }

    fn authenticate(&self, token: &str, req: &OpenRequest) -> Result<OpenedSession, ApiError> {
        let v = self.post(
            token,
            json!({
                "action": "authenticate",
                "appVersion": APP_VERSION,
                "companyFile": req.company_file,
                "productName": req.product_name,
            }),
        )?;
        serde_json::from_value(v).map_err(|_| ApiError::Server("AveeCare's answer could not be read.".into()))
    }

    fn send_request_xml(&self, token: &str, ticket: &str, req: &NextRequest) -> Result<String, ApiError> {
        let v = self.post(
            token,
            json!({
                "action": "sendRequestXML",
                "ticket": ticket,
                "strHCPResponse": req.hcp_response,
                "strCompanyFileName": req.company_file,
                "qbXMLCountry": req.country,
                "qbXMLMajorVers": req.major,
                "qbXMLMinorVers": req.minor,
            }),
        )?;
        Ok(v.get("xml").and_then(|x| x.as_str()).unwrap_or("").to_string())
    }

    fn receive_response_xml(&self, token: &str, ticket: &str, response: &str, hresult: &str, message: &str) -> Result<StepAnswer, ApiError> {
        let v = self.post(
            token,
            json!({ "action": "receiveResponseXML", "ticket": ticket, "response": response, "hresult": hresult, "message": message }),
        )?;
        Ok(StepAnswer {
            percent: v.get("percent").and_then(|p| p.as_i64()).unwrap_or(-1) as i32,
            result: v.get("result").cloned().and_then(|r| serde_json::from_value(r).ok()),
        })
    }

    fn get_last_error(&self, token: &str, ticket: &str) -> Result<String, ApiError> {
        let v = self.post(token, json!({ "action": "getLastError", "ticket": ticket }))?;
        Ok(v.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string())
    }

    fn connection_error(&self, token: &str, ticket: &str, hresult: &str, message: &str) -> Result<(), ApiError> {
        self.post(token, json!({ "action": "connectionError", "ticket": ticket, "hresult": hresult, "message": message }))?;
        Ok(())
    }

    fn close_connection(&self, token: &str, ticket: &str) -> Result<Closed, ApiError> {
        let v = self.post(token, json!({ "action": "closeConnection", "ticket": ticket }))?;
        Ok(closed_from(&v))
    }
}

/// Reads a closeConnection answer. An older server sends no results; a result
/// the app cannot read is dropped rather than failing the close.
fn closed_from(v: &Value) -> Closed {
    let message = v.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let results = v
        .get("results")
        .and_then(|r| r.as_array())
        .map(|list| list.iter().filter_map(|x| serde_json::from_value::<StepResult>(x.clone()).ok()).collect())
        .unwrap_or_default();
    Closed { message, results }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_reads_late_results_and_tolerates_an_older_server() {
        let c = closed_from(&json!({ "ok": true, "message": "done", "results": [
            { "kind": "change", "outcome": "refused", "title": "Visit on Nov 7, Alayna Wright: Changed in QuickBooks since AveeCare sent it.", "fixPath": "/payroll", "fixLabel": "Open Payroll", "statusCode": 0 },
            { "nonsense": true }
        ] }));
        assert_eq!(c.message, "done");
        assert_eq!(c.results.len(), 1);
        assert_eq!(c.results[0].outcome, "refused");
        assert_eq!(c.results[0].fix_path.as_deref(), Some("/payroll"));
        let old = closed_from(&json!({ "ok": true, "message": "done" }));
        assert!(old.results.is_empty());
    }

    #[test]
    fn errors_prefer_the_servers_sentence() {
        assert!(matches!(api_error(401, "SIGNED_OUT", ""), ApiError::SignedOut(_)));
        match api_error(403, "QBD_NOT_SET_UP", "QuickBooks Desktop is not turned on.") {
            ApiError::Refused { reason, message } => {
                assert_eq!(reason, "QBD_NOT_SET_UP");
                assert_eq!(message, "QuickBooks Desktop is not turned on.");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(api_error(502, "SESSION_FAILED", ""), ApiError::Server(_)));
        assert!(matches!(api_error(403, "UNKNOWN_TICKET", ""), ApiError::Refused { .. }));
    }

    #[test]
    fn step_result_reads_the_servers_shape() {
        let v = json!({
            "kind": "hours", "outcome": "refused", "title": "QuickBooks refused 2h for Walters, Barbara on 2026-11-02",
            "detail": "Invalid reference (code 3140)", "fixPath": "/settings?setting=qbd-caregiver-matching",
            "fixLabel": "Match caregivers in QuickBooks", "minutes": 120, "statusCode": 3140
        });
        let r: StepResult = serde_json::from_value(v).unwrap();
        assert_eq!(r.outcome, "refused");
        assert_eq!(r.fix_path.as_deref(), Some("/settings?setting=qbd-caregiver-matching"));
        assert_eq!(r.status_code, 3140);
    }
}
