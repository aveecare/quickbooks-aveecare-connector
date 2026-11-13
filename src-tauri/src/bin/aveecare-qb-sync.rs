//! Headless sync: the app's own sign-in and sync loop, without the window.
//!
//! For testing, with an emulator standing in for QuickBooks. Not shipped in
//! the installer.
//!
//!   aveecare-qb-sync --server <url> --email <email> \
//!       --password ... --emulator http://127.0.0.1:<port> [--out run.json]
//!
//! `--com` uses real QuickBooks over COM instead of the emulator.
//! The password may come from AVEECARE_PASSWORD instead of the command line.

use aveecare_qb_connector_lib::auth::SignInStep;
use aveecare_qb_connector_lib::session::{MemorySecrets, Session};
use aveecare_qb_connector_lib::sync::{run_sync, RunStatus, SyncContext, SyncEvent};
use std::sync::atomic::AtomicBool;

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let server = arg("--server").unwrap_or_else(|| "http://localhost:3000".into());
    let email = arg("--email").expect("--email");
    let password = arg("--password").or_else(|| std::env::var("AVEECARE_PASSWORD").ok()).expect("--password or AVEECARE_PASSWORD");
    let use_com = std::env::args().any(|a| a == "--com");

    let session = Session::new(&server, Box::new(MemorySecrets::default()));
    match session.sign_in(&email, &password) {
        Ok(SignInStep::Done(_)) => println!("signed in as {email}"),
        Ok(SignInStep::TotpCode { .. }) => {
            eprintln!("this login asks for an authenticator code; the headless runner does not support that");
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("sign-in failed: {e}");
            std::process::exit(2);
        }
    }
    match session.profile() {
        Ok(p) => println!("profile: {} {} <{}> role={} agency={}", p.first_name, p.last_name, p.email, p.role, p.company_name),
        Err(e) => {
            eprintln!("AveeCare refused this login: {e}");
            std::process::exit(3);
        }
    }

    let api = session.api();
    let token = || session.id_token();
    let cancel = AtomicBool::new(false);
    let ctx = SyncContext { api: &api, token: &token, cancel: &cancel, trigger: "headless", expected_file: None, allow_file: None };
    let mut print = |e: SyncEvent| match &e {
        SyncEvent::Step { index, percent, result } => match result {
            Some(r) => println!("{:>4}. [{}] {} ({percent}%)", index + 1, r.outcome, r.title),
            None => println!("{:>4}. ({percent}%)", index + 1),
        },
        other => println!("-- {}", serde_json::to_string(other).unwrap_or_default()),
    };

    let record = if use_com {
        #[cfg(windows)]
        {
            let mut t = aveecare_qb_connector_lib::qb::com::ComTransport::new();
            run_sync(&ctx, &mut t, &mut print)
        }
        #[cfg(not(windows))]
        panic!("--com needs Windows")
    } else {
        let url = arg("--emulator").expect("--emulator http://127.0.0.1:<port> (or --com)");
        let mut t = aveecare_qb_connector_lib::qb::emulator::EmulatorTransport::new(&url).unwrap_or_else(|e| panic!("{e}"));
        run_sync(&ctx, &mut t, &mut print)
    };

    println!(
        "status={:?} mode={} requests={} landed={} already={} refused={} file={} product={}",
        record.status, record.mode, record.requests, record.totals.landed, record.totals.already, record.totals.refused, record.company_file, record.product_name
    );
    if let Some(p) = &record.problem {
        println!("problem: [{}:{}] {} | {} | {}", p.source, p.code, p.title, p.fix, p.raw);
    }
    if let Some(out) = arg("--out") {
        std::fs::write(&out, serde_json::to_vec_pretty(&record).unwrap()).expect("write --out");
        println!("record written to {out}");
    }
    let ok = matches!(record.status, RunStatus::Completed | RunStatus::CompletedWithProblems | RunStatus::NothingToSend);
    std::process::exit(if ok { 0 } else { 1 });
}
