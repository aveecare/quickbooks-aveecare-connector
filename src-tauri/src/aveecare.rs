//! The AveeCare API: running payroll and billing from the Connector.
//!
//! The Connector chooses nothing itself. It calls the SAME `billingAPI` and
//! `payrollAPI` operations the AveeCare web pages call (Payroll → Send to
//! QuickBooks Desktop, Billing → Create invoices), through the same API, with
//! the person's own sign-in. The server checks the role, the agency and every
//! rule about what may be sent exactly as it does for the web app, so a pay
//! period or an invoice run sent from here is the run the web page would send.

use crate::api::{network_sentence, ApiError};
use crate::config::USER_AGENT;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Api {
    Billing,
    Payroll,
}

impl Api {
    pub fn parse(s: &str) -> Option<Api> {
        match s {
            "billing" => Some(Api::Billing),
            "payroll" => Some(Api::Payroll),
            _ => None,
        }
    }

    fn field(self) -> &'static str {
        match self {
            Api::Billing => "billingAPI",
            Api::Payroll => "payrollAPI",
        }
    }
}

/// The operations this app uses, and nothing else. The server would refuse
/// anything the person may not do anyway; the list keeps the app from growing
/// into a second way to drive every operation AveeCare has.
pub fn allowed(api: Api, operation: &str) -> bool {
    match api {
        Api::Payroll => matches!(
            operation,
            "validateQuickBooksDesktopRun"
                | "previewQuickBooksDesktopTimesheet"
                | "queueVisitsForQuickBooksDesktop"
                | "getQuickBooksDesktopSendHistory"
                | "queueQuickBooksDesktopChanges"
                | "resolveQuickBooksDesktopChange"
                // Drive time and mileage setup (ui/travel.js): the Settings
                // card's own read and save.
                | "getQuickBooksDesktopStatus"
                | "saveQuickBooksDesktopPayrollItemMap"
        ),
        Api::Billing => matches!(
            operation,
            "createInvoicesForRange"
                | "qbd-validate-invoices"
                | "qbd-queue-invoices"
                | "qbd-send-history"
                | "qbd-queue-changes"
                | "qbd-resolve-change"
                | "qbd-finish-removals"
                | "qbd-status"
                | "qbd-save-invoice-settings"
        ),
    }
}

pub struct AveeCareApi {
    url: String,
    http: reqwest::blocking::Client,
}

impl AveeCareApi {
    pub fn new(graphql_url: &str) -> Self {
        // AppSync abandons a request at 30 seconds; a little over that so its
        // own answer, not ours, is what the window shows.
        let http = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(45))
            .build()
            .expect("http client");
        Self { url: graphql_url.to_string(), http }
    }

    /// Run one operation. Returns the operation's `data` on success.
    pub fn call(&self, access_token: &str, api: Api, operation: &str, input: &Value) -> Result<Value, ApiError> {
        if !allowed(api, operation) {
            return Err(ApiError::Refused { reason: "NotAllowed".into(), message: "The Connector does not run that.".into() });
        }
        let field = api.field();
        let body = json!({
            "query": format!("query Run($operation: String!, $input: AWSJSON) {{ {field}(operation: $operation, input: $input) }}"),
            "variables": { "operation": operation, "input": input.to_string() },
        });
        let res = self
            .http
            .post(&self.url)
            // AppSync's Cognito authorizer takes the raw token, no "Bearer".
            .header("Authorization", access_token)
            .json(&body)
            .send()
            .map_err(|e| ApiError::Network(network_sentence(&e)))?;
        let status = res.status().as_u16();
        let v: Value = res.json().unwrap_or(Value::Null);
        read_answer(status, field, &v)
    }
}

/// Turn AppSync's answer into the operation's data, or a sentence.
pub fn read_answer(status: u16, field: &str, v: &Value) -> Result<Value, ApiError> {
    let first_error = v
        .get("errors")
        .and_then(|e| e.as_array())
        .and_then(|a| a.first())
        .map(|e| (
            e.get("errorType").and_then(|t| t.as_str()).unwrap_or("").to_string(),
            e.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string(),
        ));
    if status == 401 || first_error.as_ref().is_some_and(|(t, _)| t.contains("Unauthorized")) {
        return Err(ApiError::SignedOut("Your AveeCare sign-in has ended. Sign in again.".into()));
    }
    if let Some((_, message)) = &first_error {
        if message.contains("timed out") {
            return Err(ApiError::Server(
                "AveeCare did not answer in time. It may still have done this: check AveeCare before trying again.".into(),
            ));
        }
        return Err(ApiError::Server(if message.is_empty() { "AveeCare had a problem. Try again in a minute.".into() } else { message.clone() }));
    }
    if !(200..300).contains(&status) {
        return Err(ApiError::Server("AveeCare had a problem. Try again in a minute.".into()));
    }
    // a.json() comes back as a JSON string, and the handlers return a string
    // too, so it can be encoded more than once; unwrap as the web client does.
    let mut envelope = v.get("data").and_then(|d| d.get(field)).cloned().unwrap_or(Value::Null);
    for _ in 0..4 {
        match envelope {
            Value::String(s) => envelope = serde_json::from_str::<Value>(&s).unwrap_or(Value::Null),
            _ => break,
        }
    }
    if envelope.get("success").and_then(|s| s.as_bool()) == Some(true) {
        return Ok(envelope.get("data").cloned().unwrap_or(Value::Null));
    }
    let message = envelope
        .get("error")
        .and_then(|e| e.as_str())
        .filter(|m| !m.is_empty())
        .unwrap_or("AveeCare could not do that.")
        .to_string();
    // The server's own code (CAPABILITY_OFF when Billing is turned off for this
    // person) lets the window tell a permission refusal from any other.
    let reason = envelope
        .get("code")
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty())
        .unwrap_or("Refused")
        .to_string();
    Err(ApiError::Refused { reason, message })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_listed_operations_run() {
        assert!(allowed(Api::Payroll, "queueVisitsForQuickBooksDesktop"));
        assert!(allowed(Api::Payroll, "saveQuickBooksDesktopPayrollItemMap"));
        assert!(allowed(Api::Billing, "qbd-save-invoice-settings"));
        // Other settings stay on the web page.
        assert!(!allowed(Api::Payroll, "saveQuickBooksDesktopEmployeeMap"));
        assert!(!allowed(Api::Payroll, "disconnectQuickBooksDesktop"));
        assert!(allowed(Api::Billing, "createInvoicesForRange"));
        assert!(!allowed(Api::Billing, "deleteInvoice"));
        assert!(!allowed(Api::Payroll, "createInvoicesForRange"));
        let r = AveeCareApi::new("http://127.0.0.1:1/graphql").call("t", Api::Billing, "deleteInvoice", &json!({}));
        assert!(matches!(r, Err(ApiError::Refused { .. })));
    }

    #[test]
    fn success_returns_the_data_from_a_string_envelope() {
        let v = json!({ "data": { "payrollAPI": "{\"success\":true,\"data\":{\"ready\":3}}" } });
        assert_eq!(read_answer(200, "payrollAPI", &v).unwrap(), json!({ "ready": 3 }));
    }

    #[test]
    fn a_refusal_keeps_the_servers_code() {
        let body = serde_json::json!({ "success": false, "error": "Billing is turned off for you.", "code": "CAPABILITY_OFF" }).to_string();
        let v = serde_json::json!({ "data": { "billingAPI": body } });
        match read_answer(200, "billingAPI", &v) {
            Err(ApiError::Refused { reason, .. }) => assert_eq!(reason, "CAPABILITY_OFF"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        let plain = serde_json::json!({ "data": { "billingAPI": serde_json::json!({ "success": false, "error": "No" }).to_string() } });
        assert!(matches!(read_answer(200, "billingAPI", &plain), Err(ApiError::Refused { reason, .. }) if reason == "Refused"));
    }

    #[test]
    fn a_doubly_encoded_envelope_is_read() {
        let inner = json!({ "success": true, "data": { "ready": 2 } }).to_string();
        let v = json!({ "data": { "payrollAPI": serde_json::to_string(&inner).unwrap() } });
        assert_eq!(read_answer(200, "payrollAPI", &v).unwrap(), json!({ "ready": 2 }));
    }

    #[test]
    fn a_refusal_carries_the_server_sentence() {
        let v = json!({ "data": { "billingAPI": "{\"success\":false,\"error\":\"Pick a date range first\"}" } });
        match read_answer(200, "billingAPI", &v) {
            Err(ApiError::Refused { message, .. }) => assert_eq!(message, "Pick a date range first"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_unauthorized_answer_signs_out() {
        let v = json!({ "errors": [{ "errorType": "UnauthorizedException", "message": "Valid authorization header not provided." }] });
        assert!(matches!(read_answer(401, "billingAPI", &v), Err(ApiError::SignedOut(_))));
        assert!(matches!(read_answer(200, "billingAPI", &v), Err(ApiError::SignedOut(_))));
    }

    #[test]
    fn a_timeout_says_it_may_have_happened() {
        let v = json!({ "data": { "billingAPI": null }, "errors": [{ "errorType": "ExecutionTimeout", "message": "Execution timed out." }] });
        match read_answer(200, "billingAPI", &v) {
            Err(ApiError::Server(m)) => assert!(m.contains("may still have done this")),
            other => panic!("{other:?}"),
        }
    }
}
