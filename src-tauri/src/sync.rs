//! One sync: carry AveeCare's qbXML requests to QuickBooks and QuickBooks'
//! answers back, exactly as Intuit's Web Connector does, and keep a plain
//! record of what happened.
//!
//! The app decides nothing about payroll or billing. The server plans every
//! request, decides whether a refusal skips a line or stops the run, and
//! records what landed; it also describes each step in words, which is what
//! the activity list shows.

use crate::api::{ApiError, ConnectorApi, NextRequest, OpenRequest, Plan, StepResult};
use crate::config::{PROBE_QBXML_VERSION, QB_APP_ID, QB_APP_NAME};
use crate::qb::host::{host_query_xml, parse_host, split_version, HostInfo};
use crate::qb::{QbError, QbErrorKind, QbTransport};
use crate::store::same_file;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

/// The most requests one run will carry. A real fortnight is a few hundred;
/// this only stops a server bug from looping forever.
const MAX_STEPS: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunStatus {
    /// Everything AveeCare had waiting went in.
    Completed,
    /// It finished, and QuickBooks refused one or more items.
    CompletedWithProblems,
    /// Nothing was waiting; the Connector refreshed AveeCare's copy of the QuickBooks lists.
    NothingToSend,
    /// It started and stopped partway. Whatever landed before is safe.
    Stopped,
    /// It could not start.
    Failed,
    /// The person pressed Stop.
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunProblem {
    /// "quickbooks" or "aveecare": whose side the problem is on.
    pub source: String,
    /// QbErrorKind or the server's refusal code, for the UI to choose an action.
    pub code: String,
    pub title: String,
    pub fix: String,
    #[serde(default)]
    pub raw: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunTotals {
    pub landed: u32,
    pub already: u32,
    pub refused: u32,
    pub hours_minutes_landed: f64,
    pub invoices_landed: u32,
    pub invoice_cents_landed: i64,
    pub payments_landed: u32,
    pub payment_cents_landed: i64,
    /// Mileage reimbursement checks (only when the agency turned them on).
    pub mileage_checks_landed: u32,
    pub mileage_cents_landed: i64,
    /// Undo and update-in-place steps QuickBooks accepted (deletes, voids, edits).
    #[serde(default)]
    pub changes_landed: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub id: String,
    pub started_at: String,
    pub finished_at: String,
    pub trigger: String,
    pub status: RunStatus,
    /// "timesheet", "discovery", "configure", ... as the server chose.
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub plan: Plan,
    #[serde(default)]
    pub company_file: String,
    #[serde(default)]
    pub product_name: String,
    #[serde(default)]
    pub qbxml_version: String,
    #[serde(default)]
    pub steps: Vec<StepResult>,
    #[serde(default)]
    pub totals: RunTotals,
    #[serde(default)]
    pub problem: Option<RunProblem>,
    /// The server's closing sentence.
    #[serde(default)]
    pub closing: String,
    /// How many requests went to QuickBooks.
    #[serde(default)]
    pub requests: u32,
}

/// What the window shows while a sync runs.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum SyncEvent {
    ConnectingToQuickBooks,
    #[serde(rename_all = "camelCase")]
    QuickBooksOpen { company_file: String, product_name: String },
    AskingAveeCare,
    #[serde(rename_all = "camelCase")]
    Started { mode: String, plan: Plan },
    #[serde(rename_all = "camelCase")]
    Step { index: u32, percent: i32, result: Option<StepResult> },
    Finishing,
}

pub struct SyncContext<'a> {
    pub api: &'a dyn ConnectorApi,
    /// A current ID token, refreshed when near expiry. Called before every request.
    pub token: &'a dyn Fn() -> Result<String, ApiError>,
    pub cancel: &'a AtomicBool,
    pub trigger: &'a str,
    /// The company file the last good sync for this agency used. A different
    /// open file stops the run before AveeCare is asked for anything, unless
    /// the person has said this file is right (`allow_file`).
    pub expected_file: Option<&'a str>,
    pub allow_file: Option<&'a str>,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn qb_problem(e: &QbError) -> RunProblem {
    RunProblem {
        source: "quickbooks".into(),
        code: serde_json::to_value(e.kind).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_else(|| "other".into()),
        title: e.title.clone(),
        fix: e.fix.clone(),
        raw: match &e.hresult {
            Some(h) if e.raw.is_empty() => h.clone(),
            Some(h) => format!("{} ({h})", e.raw),
            None => e.raw.clone(),
        },
    }
}

fn api_problem(e: &ApiError) -> RunProblem {
    let (code, title, fix) = match e {
        ApiError::SignedOut(m) => ("SIGNED_OUT".to_string(), m.clone(), "Sign in again, then sync.".to_string()),
        ApiError::Refused { reason, message } => (reason.clone(), message.clone(), refusal_fix(reason).to_string()),
        ApiError::Network(m) => ("NETWORK".into(), m.clone(), "Nothing was lost. Sync again when you are back online.".into()),
        ApiError::Server(m) => ("SERVER".into(), m.clone(), "Nothing was lost. Sync again in a minute.".into()),
    };
    RunProblem { source: "aveecare".into(), code, title, fix, raw: String::new() }
}

fn refusal_fix(reason: &str) -> &'static str {
    match reason {
        "QBD_NOT_SET_UP" => "In AveeCare, open Settings, then QuickBooks Desktop, and turn it on.",
        "ROLE_NOT_ALLOWED" | "CAPABILITY_OFF" => "Ask an owner of your AveeCare account to give you Payroll or Billing access.",
        "ACCOUNT_DISABLED" => "Ask your office to turn your login back on.",
        "UNKNOWN_TICKET" => "Sync again.",
        _ => "",
    }
}

fn totals_of(steps: &[StepResult]) -> RunTotals {
    let mut t = RunTotals::default();
    for s in steps {
        match s.outcome.as_str() {
            "landed" => {
                t.landed += 1;
                match s.kind.as_str() {
                    "hours" => t.hours_minutes_landed += s.minutes.unwrap_or(0.0),
                    "invoice" => {
                        t.invoices_landed += 1;
                        t.invoice_cents_landed += s.amount_cents.unwrap_or(0);
                    }
                    "payment" => {
                        t.payments_landed += 1;
                        t.payment_cents_landed += s.amount_cents.unwrap_or(0);
                    }
                    "mileage" => {
                        t.mileage_checks_landed += 1;
                        t.mileage_cents_landed += s.amount_cents.unwrap_or(0);
                    }
                    "change" => t.changes_landed += 1,
                    _ => {}
                }
            }
            "already" => t.already += 1,
            "refused" | "stopped" => t.refused += 1,
            _ => {}
        }
    }
    t
}

/// Run one sync. Never panics on QuickBooks or AveeCare failures: every
/// outcome comes back as a record.
pub fn run_sync(ctx: &SyncContext, transport: &mut dyn QbTransport, on_event: &mut dyn FnMut(SyncEvent)) -> RunRecord {
    let mut rec = RunRecord {
        id: uuid::Uuid::new_v4().to_string(),
        started_at: now(),
        finished_at: String::new(),
        trigger: ctx.trigger.to_string(),
        status: RunStatus::Failed,
        mode: String::new(),
        plan: Plan::default(),
        company_file: String::new(),
        product_name: String::new(),
        qbxml_version: String::new(),
        steps: Vec::new(),
        totals: RunTotals::default(),
        problem: None,
        closing: String::new(),
        requests: 0,
    };
    let finish = |mut rec: RunRecord| {
        rec.totals = totals_of(&rec.steps);
        rec.finished_at = now();
        rec
    };

    // 1. QuickBooks first: if it is not ready there is nothing to ask AveeCare for.
    on_event(SyncEvent::ConnectingToQuickBooks);
    let opened = match transport.open(QB_APP_ID, QB_APP_NAME) {
        Ok(f) => f,
        Err(e) => {
            transport.close();
            rec.problem = Some(qb_problem(&e));
            return finish(rec);
        }
    };
    rec.company_file = opened.company_file.clone();
    if let Some(expected) = ctx.expected_file {
        let allowed = ctx.allow_file.map(|a| same_file(a, &rec.company_file)).unwrap_or(false);
        if !expected.is_empty() && !rec.company_file.is_empty() && !same_file(expected, &rec.company_file) && !allowed {
            transport.close();
            rec.problem = Some(RunProblem {
                source: "quickbooks".into(),
                code: "wrongCompanyFile".into(),
                title: "QuickBooks has a different company file open than last time.".into(),
                fix: format!(
                    "Last time AveeCare synced with {expected}. Open that file in QuickBooks and sync again, or, if this file is now the right one, choose \"Use {}\".",
                    rec.company_file.rsplit(['\\', '/']).next().unwrap_or(&rec.company_file)
                ),
                raw: rec.company_file.clone(),
            });
            return finish(rec);
        }
    }

    // 2. What QuickBooks is: edition and the qbXML version this file speaks.
    let host: HostInfo = match transport.process(&host_query_xml(PROBE_QBXML_VERSION)) {
        Ok(xml) => parse_host(&xml).unwrap_or_default(),
        Err(e) if e.is_line_level() => HostInfo::default(),
        Err(e) => {
            transport.close();
            rec.problem = Some(qb_problem(&e));
            return finish(rec);
        }
    };
    rec.product_name = host.product_name.clone();
    let version = if host.qbxml_version.is_empty() { PROBE_QBXML_VERSION.to_string() } else { host.qbxml_version.clone() };
    rec.qbxml_version = version.clone();
    let (major, minor) = split_version(&version);
    on_event(SyncEvent::QuickBooksOpen { company_file: rec.company_file.clone(), product_name: rec.product_name.clone() });

    // 3. Open the session on AveeCare: the Web Connector's `authenticate`.
    on_event(SyncEvent::AskingAveeCare);
    let token = match (ctx.token)() {
        Ok(t) => t,
        Err(e) => {
            transport.close();
            rec.problem = Some(api_problem(&e));
            return finish(rec);
        }
    };
    let session = match ctx.api.authenticate(&token, &OpenRequest { company_file: rec.company_file.clone(), product_name: rec.product_name.clone() }) {
        Ok(s) => s,
        Err(e) => {
            transport.close();
            rec.problem = Some(api_problem(&e));
            return finish(rec);
        }
    };
    rec.mode = session.mode.clone();
    rec.plan = session.plan.clone();
    on_event(SyncEvent::Started { mode: session.mode.clone(), plan: session.plan.clone() });
    let ticket = session.ticket;

    // 4. The conversation.
    let mut stopped: Option<RunProblem> = None;
    let mut canceled = false;
    let mut server_said_stop = false;
    let mut first = true;
    for index in 0..MAX_STEPS {
        if ctx.cancel.load(Ordering::SeqCst) {
            canceled = true;
            break;
        }
        let token = match (ctx.token)() {
            Ok(t) => t,
            Err(e) => {
                stopped = Some(api_problem(&e));
                break;
            }
        };
        let request = match ctx.api.send_request_xml(
            &token,
            &ticket,
            &NextRequest {
                // The Web Connector sends its HostQuery answer on the first call only.
                hcp_response: if first { host_query_xml(&version) } else { String::new() },
                company_file: rec.company_file.clone(),
                country: if host.country.is_empty() { "US".into() } else { host.country.clone() },
                major: major.clone(),
                minor: minor.clone(),
            },
        ) {
            Ok(x) => x,
            Err(e) => {
                stopped = Some(api_problem(&e));
                break;
            }
        };
        first = false;
        if request.is_empty() {
            // Nothing more to send, or the server stopped the run: getLastError says which.
            server_said_stop = true;
            break;
        }

        rec.requests += 1;
        let (response, hresult, message) = match transport.process(&request) {
            Ok(r) => (r, String::new(), String::new()),
            Err(e) if e.is_line_level() => (String::new(), e.hresult.clone().unwrap_or_default(), e.raw.clone()),
            Err(e) => {
                // QuickBooks itself went away: tell AveeCare, as the Web Connector does.
                let _ = ctx.api.connection_error(&token, &ticket, e.hresult.as_deref().unwrap_or(""), &e.raw);
                stopped = Some(qb_problem(&e));
                break;
            }
        };

        let token = match (ctx.token)() {
            Ok(t) => t,
            Err(e) => {
                stopped = Some(api_problem(&e));
                break;
            }
        };
        match ctx.api.receive_response_xml(&token, &ticket, &response, &hresult, &message) {
            Ok(answer) => {
                if let Some(r) = answer.result.clone() {
                    rec.steps.push(r);
                }
                on_event(SyncEvent::Step { index: index as u32, percent: answer.percent, result: answer.result });
                if answer.percent < 0 {
                    server_said_stop = true;
                    break;
                }
                if answer.percent >= 100 {
                    break;
                }
            }
            Err(e) => {
                stopped = Some(api_problem(&e));
                break;
            }
        }
    }

    // 5. Why it ended, then close: always, so the session is released.
    on_event(SyncEvent::Finishing);
    let token = (ctx.token)().unwrap_or_default();
    if server_said_stop && stopped.is_none() {
        if let Ok(msg) = ctx.api.get_last_error(&token, &ticket) {
            if !msg.trim().is_empty() {
                stopped = Some(RunProblem {
                    source: "aveecare".into(),
                    code: "RUN_STOPPED".into(),
                    title: msg.trim().to_string(),
                    fix: "Anything that landed before this is safe and will not be sent twice. Fix what is named above, then sync again.".into(),
                    raw: String::new(),
                });
            }
        }
    }
    if let Ok(closed) = ctx.api.close_connection(&token, &ticket) {
        rec.closing = closed.message;
        // Records decided without a request (a change held because someone
        // edited it in QuickBooks by hand) count like any step, so a sync that
        // held one never reads as "Everything went in".
        for r in closed.results {
            on_event(SyncEvent::Step { index: rec.requests, percent: 100, result: Some(r.clone()) });
            rec.steps.push(r);
        }
    }
    transport.close();

    rec.problem = stopped;
    let refused = rec.steps.iter().any(|s| s.outcome == "refused" || s.outcome == "stopped");
    let wrote = rec.steps.iter().any(|s| s.outcome == "landed" || s.outcome == "already" || s.outcome == "refused");
    rec.status = if canceled {
        RunStatus::Canceled
    } else if rec.problem.is_some() {
        RunStatus::Stopped
    } else if refused {
        RunStatus::CompletedWithProblems
    } else if rec.mode == "discovery" && !wrote {
        RunStatus::NothingToSend
    } else {
        RunStatus::Completed
    };
    finish(rec)
}

/// Before touching COM: is QuickBooks there at all, and can we reach it?
/// Returns the problem to show, or None to go ahead.
pub fn preflight(running: bool, qb_elevated: Option<bool>, self_elevated: bool) -> Option<QbError> {
    if !running {
        return Some(QbError::new(QbErrorKind::NotRunning, "QuickBooks is not open.", "Open QuickBooks Desktop and your company file, then sync again."));
    }
    if qb_elevated == Some(true) && !self_elevated {
        return Some(QbError::new(
            QbErrorKind::ElevationMismatch,
            "QuickBooks is running as administrator, so the Connector cannot reach it.",
            "Close QuickBooks and open it normally (not \"Run as administrator\"). Or press \"Restart as administrator\" to run the Connector the same way.",
        ));
    }
    None
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::api::{Closed, OpenedSession, Profile, StepAnswer};
    use crate::qb::fake::{ok_response, FakeTransport};
    use crate::qb::{classify_hresult, QbError};
    use std::sync::Mutex;

    /// A server that hands out scripted requests and records every call.
    pub struct FakeApi {
        pub requests: Mutex<Vec<String>>,
        pub results: Mutex<Vec<(i32, Option<StepResult>)>>,
        pub calls: Mutex<Vec<String>>,
        pub responses: Mutex<Vec<(String, String, String)>>,
        pub last_error: String,
        pub auth_error: Option<ApiError>,
        pub mode: String,
        pub closing_results: Mutex<Vec<StepResult>>,
    }

    impl FakeApi {
        pub fn new(requests: &[&str], results: Vec<(i32, Option<StepResult>)>) -> Self {
            Self {
                requests: Mutex::new(requests.iter().rev().map(|s| s.to_string()).collect()),
                results: Mutex::new(results.into_iter().rev().collect()),
                calls: Mutex::new(vec![]),
                responses: Mutex::new(vec![]),
                last_error: String::new(),
                auth_error: None,
                mode: "timesheet".into(),
                closing_results: Mutex::new(vec![]),
            }
        }
        fn note(&self, c: &str) {
            self.calls.lock().unwrap().push(c.into());
        }
    }

    impl ConnectorApi for FakeApi {
        fn profile(&self, _t: &str) -> Result<Profile, ApiError> {
            Ok(Profile::default())
        }
        fn authenticate(&self, t: &str, req: &OpenRequest) -> Result<OpenedSession, ApiError> {
            assert_eq!(t, "tok");
            self.note(&format!("authenticate {}", req.company_file));
            if let Some(e) = self.auth_error.clone() {
                return Err(e);
            }
            Ok(OpenedSession { ticket: "T1".into(), mode: self.mode.clone(), plan: Plan { timesheet_lines: 2, visits: 2, invoices: 1, payments: 0, ..Default::default() } })
        }
        fn send_request_xml(&self, _t: &str, ticket: &str, req: &NextRequest) -> Result<String, ApiError> {
            assert_eq!(ticket, "T1");
            self.note(&format!("send {}.{}", req.major, req.minor));
            Ok(self.requests.lock().unwrap().pop().unwrap_or_default())
        }
        fn receive_response_xml(&self, _t: &str, _ticket: &str, response: &str, hresult: &str, message: &str) -> Result<StepAnswer, ApiError> {
            self.note("receive");
            self.responses.lock().unwrap().push((response.into(), hresult.into(), message.into()));
            let (percent, result) = self.results.lock().unwrap().pop().unwrap_or((100, None));
            Ok(StepAnswer { percent, result })
        }
        fn get_last_error(&self, _t: &str, _ticket: &str) -> Result<String, ApiError> {
            self.note("getLastError");
            Ok(self.last_error.clone())
        }
        fn connection_error(&self, _t: &str, _ticket: &str, hresult: &str, _m: &str) -> Result<(), ApiError> {
            self.note(&format!("connectionError {hresult}"));
            Ok(())
        }
        fn close_connection(&self, _t: &str, _ticket: &str) -> Result<Closed, ApiError> {
            self.note("close");
            Ok(Closed { message: "AveeCare sync completed successfully.".into(), results: self.closing_results.lock().unwrap().clone() })
        }
    }

    fn step(kind: &str, outcome: &str, minutes: Option<f64>, cents: Option<i64>) -> StepResult {
        StepResult {
            kind: kind.into(),
            outcome: outcome.into(),
            title: format!("{kind} {outcome}"),
            detail: None,
            warning: None,
            fix_path: None,
            fix_label: None,
            amount_cents: cents,
            minutes,
            txn_id: None,
            status_code: 0,
        }
    }

    fn run(api: &FakeApi, qb: &mut FakeTransport) -> (RunRecord, Vec<SyncEvent>) {
        let cancel = AtomicBool::new(false);
        let token = || Ok("tok".to_string());
        let ctx = SyncContext { api, token: &token, cancel: &cancel, trigger: "manual", expected_file: None, allow_file: None };
        let mut events = vec![];
        let rec = run_sync(&ctx, qb, &mut |e| events.push(e));
        (rec, events)
    }

    #[test]
    fn a_change_held_at_close_makes_the_run_need_attention() {
        let api = FakeApi::new(&["<r1/>"], vec![(100, Some(step("change", "landed", None, None)))]);
        *api.closing_results.lock().unwrap() = vec![step("change", "refused", None, None), step("change", "already", None, None)];
        let mut qb = FakeTransport::new(r"C:\Files\Angels.QBW").answer(Ok(ok_response("TimeTrackingAddRs")));
        let (rec, events) = run(&api, &mut qb);
        assert_eq!(rec.status, RunStatus::CompletedWithProblems);
        assert_eq!(rec.totals.changes_landed, 1);
        assert_eq!(rec.totals.refused, 1);
        assert_eq!(rec.totals.already, 1);
        assert_eq!(rec.steps.len(), 3);
        assert!(events.iter().filter(|e| matches!(e, SyncEvent::Step { .. })).count() >= 3);
    }

    #[test]
    fn counts_undo_and_update_changes_that_landed() {
        let api = FakeApi::new(
            &["<TxnDelRq/>", "<InvoiceModRq/>"],
            vec![(50, Some(step("change", "landed", None, None))), (100, Some(step("change", "landed", None, None)))],
        );
        let mut qb = FakeTransport::new(r"C:\Files\Angels.QBW")
            .answer(Ok(ok_response("TxnDelRs")))
            .answer(Ok(ok_response("InvoiceModRs")));
        let (rec, _) = run(&api, &mut qb);
        assert_eq!(rec.status, RunStatus::Completed);
        assert_eq!(rec.totals.changes_landed, 2);
        assert_eq!(rec.totals.landed, 2);
    }

    #[test]
    fn carries_every_request_and_counts_what_landed() {
        let api = FakeApi::new(
            &["<TimeTrackingAddRq/>", "<InvoiceAddRq/>"],
            vec![(50, Some(step("hours", "landed", Some(120.0), None))), (100, Some(step("invoice", "landed", None, Some(12345))))],
        );
        let mut qb = FakeTransport::new("C:\\Files\\Angels.QBW")
            .answer(Ok(ok_response("TimeTrackingAddRs")))
            .answer(Ok(ok_response("InvoiceAddRs")));
        let log = qb.log.clone();
        let (rec, events) = run(&api, &mut qb);

        assert_eq!(rec.status, RunStatus::Completed);
        assert_eq!(rec.company_file, "C:\\Files\\Angels.QBW");
        assert_eq!(rec.product_name, "QuickBooks Desktop Pro 2024");
        assert_eq!(rec.qbxml_version, "16.0");
        assert_eq!(rec.requests, 2);
        assert_eq!(rec.totals.landed, 2);
        assert_eq!(rec.totals.hours_minutes_landed, 120.0);
        assert_eq!(rec.totals.invoice_cents_landed, 12345);
        assert_eq!(rec.closing, "AveeCare sync completed successfully.");
        // The file's own version is spoken, not the probe's.
        assert!(api.calls.lock().unwrap().iter().any(|c| c == "send 16.0"));
        // QuickBooks' answers reach AveeCare unaltered.
        let responses = api.responses.lock().unwrap();
        assert!(responses[0].0.contains("TimeTrackingAddRs"));
        // HostQuery, then the two requests, then closed once.
        let l = log.lock().unwrap();
        assert_eq!(l.requests.len(), 3);
        assert_eq!(l.requests[1], "<TimeTrackingAddRq/>");
        assert_eq!(l.closed, 1);
        assert!(matches!(events.first(), Some(SyncEvent::ConnectingToQuickBooks)));
        assert!(events.iter().any(|e| matches!(e, SyncEvent::Step { percent: 100, .. })));
        assert_eq!(api.calls.lock().unwrap().last().map(String::as_str), Some("close"));
    }

    #[test]
    fn quickbooks_not_ready_never_opens_an_aveecare_session() {
        let api = FakeApi::new(&[], vec![]);
        let mut qb = FakeTransport::new("").failing_open(classify_hresult(0x80040416, "If QuickBooks is not running, a company file must be specified."));
        let (rec, _) = run(&api, &mut qb);
        assert_eq!(rec.status, RunStatus::Failed);
        let p = rec.problem.unwrap();
        assert_eq!(p.source, "quickbooks");
        assert_eq!(p.code, "noCompanyOpen");
        assert!(p.raw.contains("0x80040416"));
        assert!(api.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_line_quickbooks_cannot_parse_goes_back_as_an_hresult_and_the_run_carries_on() {
        let api = FakeApi::new(
            &["<InvoiceAddRq/>", "<TimeTrackingAddRq/>"],
            vec![(50, Some(step("invoice", "refused", None, Some(500)))), (100, Some(step("hours", "landed", Some(60.0), None)))],
        );
        let mut qb = FakeTransport::new("C:\\a.QBW")
            .answer(Err(classify_hresult(0x80040400, "QuickBooks found an error when parsing the provided XML text stream.")))
            .answer(Ok(ok_response("TimeTrackingAddRs")));
        let (rec, _) = run(&api, &mut qb);
        let responses = api.responses.lock().unwrap();
        assert_eq!(responses[0].0, "");
        assert_eq!(responses[0].1, "0x80040400");
        assert!(responses[0].2.contains("parsing"));
        assert_eq!(rec.status, RunStatus::CompletedWithProblems);
        assert_eq!(rec.totals.refused, 1);
        assert_eq!(rec.totals.landed, 1);
        assert!(rec.problem.is_none());
    }

    #[test]
    fn quickbooks_going_away_is_reported_to_aveecare_and_the_session_closed() {
        let api = FakeApi::new(&["<TimeTrackingAddRq/>"], vec![]);
        let mut qb = FakeTransport::new("C:\\a.QBW").answer(Err(classify_hresult(0x800706BA, "The RPC server is unavailable.")));
        let (rec, _) = run(&api, &mut qb);
        assert_eq!(rec.status, RunStatus::Stopped);
        assert_eq!(rec.problem.as_ref().unwrap().code, "connectionLost");
        let calls = api.calls.lock().unwrap();
        assert!(calls.iter().any(|c| c == "connectionError 0x800706BA"));
        assert_eq!(calls.last().map(String::as_str), Some("close"));
    }

    #[test]
    fn a_server_stop_reads_get_last_error() {
        let mut api = FakeApi::new(&["<TimeTrackingAddRq/>"], vec![(-1, Some(step("hours", "stopped", None, None)))]);
        api.last_error = "TimeTrackingAdd failed: Invalid reference (code 3140)".into();
        let mut qb = FakeTransport::new("C:\\a.QBW");
        let (rec, _) = run(&api, &mut qb);
        assert_eq!(rec.status, RunStatus::Stopped);
        assert!(rec.problem.unwrap().title.contains("3140"));
    }

    #[test]
    fn nothing_queued_is_a_list_refresh_not_a_success_claim() {
        let mut api = FakeApi::new(&["<HostQueryRq/>", "<EmployeeQueryRq/>"], vec![(50, Some(step("read", "read", None, None))), (100, Some(step("read", "read", None, None)))]);
        api.mode = "discovery".into();
        let mut qb = FakeTransport::new("C:\\a.QBW");
        let (rec, _) = run(&api, &mut qb);
        assert_eq!(rec.status, RunStatus::NothingToSend);
    }

    #[test]
    fn a_refused_sign_in_carries_the_servers_sentence() {
        let mut api = FakeApi::new(&[], vec![]);
        api.auth_error = Some(ApiError::Refused { reason: "QBD_NOT_SET_UP".into(), message: "QuickBooks Desktop is not turned on for your agency yet.".into() });
        let mut qb = FakeTransport::new("C:\\a.QBW");
        let log = qb.log.clone();
        let (rec, _) = run(&api, &mut qb);
        assert_eq!(rec.status, RunStatus::Failed);
        let p = rec.problem.unwrap();
        assert_eq!(p.source, "aveecare");
        assert!(p.title.contains("not turned on"));
        assert!(p.fix.contains("Settings"));
        assert_eq!(log.lock().unwrap().closed, 1);
    }

    #[test]
    fn stop_closes_the_session_cleanly() {
        let api = FakeApi::new(&["<TimeTrackingAddRq/>", "<TimeTrackingAddRq/>"], vec![(50, None), (100, None)]);
        let mut qb = FakeTransport::new("C:\\a.QBW");
        let cancel = AtomicBool::new(false);
        let token = || Ok("tok".to_string());
        let ctx = SyncContext { api: &api, token: &token, cancel: &cancel, trigger: "manual", expected_file: None, allow_file: None };
        let rec = run_sync(&ctx, &mut qb, &mut |e| {
            if matches!(e, SyncEvent::Step { .. }) {
                cancel.store(true, Ordering::SeqCst);
            }
        });
        assert_eq!(rec.status, RunStatus::Canceled);
        assert_eq!(rec.requests, 1);
        assert_eq!(api.calls.lock().unwrap().last().map(String::as_str), Some("close"));
    }

    #[test]
    fn a_different_company_file_stops_before_aveecare_is_asked() {
        let api = FakeApi::new(&["<TimeTrackingAddRq/>"], vec![]);
        let cancel = AtomicBool::new(false);
        let token = || Ok("tok".to_string());
        let mut qb = FakeTransport::new(r"C:\Q\Sample.QBW");
        let ctx = SyncContext { api: &api, token: &token, cancel: &cancel, trigger: "manual", expected_file: Some(r"C:\Q\Angels.QBW"), allow_file: None };
        let rec = run_sync(&ctx, &mut qb, &mut |_| {});
        assert_eq!(rec.status, RunStatus::Failed);
        assert_eq!(rec.problem.as_ref().unwrap().code, "wrongCompanyFile");
        assert!(api.calls.lock().unwrap().is_empty());

        // Confirmed by the person: it goes ahead.
        let mut qb = FakeTransport::new(r"C:\Q\Sample.QBW");
        let ctx = SyncContext { allow_file: Some(r"c:\q\sample.qbw"), ..ctx };
        let rec = run_sync(&ctx, &mut qb, &mut |_| {});
        assert_eq!(rec.status, RunStatus::Completed);
    }

    #[test]
    fn preflight_names_the_real_problem() {
        assert_eq!(preflight(false, None, false).unwrap().kind, QbErrorKind::NotRunning);
        assert_eq!(preflight(true, Some(true), false).unwrap().kind, QbErrorKind::ElevationMismatch);
        assert!(preflight(true, Some(true), true).is_none());
        assert!(preflight(true, Some(false), false).is_none());
        assert!(preflight(true, None, false).is_none());
        let _unused: Option<QbError> = None;
    }

    #[test]
    fn mileage_checks_count_in_the_totals_and_old_history_still_reads() {
        let step = |kind: &str, outcome: &str, cents: i64| StepResult {
            kind: kind.into(), outcome: outcome.into(), title: String::new(), detail: None, warning: None,
            fix_path: None, fix_label: None, amount_cents: Some(cents), minutes: None, txn_id: None, status_code: 0,
        };
        let t = totals_of(&[step("mileage", "landed", 4350), step("mileage", "already", 900), step("read", "read", 0)]);
        assert_eq!(t.mileage_checks_landed, 1);
        assert_eq!(t.mileage_cents_landed, 4350);
        assert_eq!(t.already, 1);
        // History written by a build without the mileage fields still loads.
        let old: RunTotals = serde_json::from_str(r#"{"landed":2,"already":0,"refused":0,"hoursMinutesLanded":60,"invoicesLanded":1,"invoiceCentsLanded":100,"paymentsLanded":0,"paymentCentsLanded":0}"#).unwrap();
        assert_eq!(old.landed, 2);
        assert_eq!(old.mileage_checks_landed, 0);
    }
}
