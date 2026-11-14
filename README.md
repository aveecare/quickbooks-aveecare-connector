# AveeCare QuickBooks Connector

A Windows desktop app that sends an agency's AveeCare payroll hours, invoices and payments into
**QuickBooks Desktop**. It replaces the Intuit Web Connector for AveeCare agencies: no `.qwc` file,
no Web Connector password, and every send shows what landed and what QuickBooks refused.

- Sign in with your AveeCare login (the same email, password and authenticator code as the web app).
- Only office staff who run payroll or billing can connect. AveeCare's server decides, not the app.
- The Connector talks to QuickBooks on this computer through Intuit's own QuickBooks SDK
  interface (`QBXMLRP2.RequestProcessor`), the same one the Web Connector uses.
- It carries requests between AveeCare and QuickBooks; it never builds payroll or invoices itself.
  All of that logic lives on the AveeCare server, shared with the Web Connector path, so both
  routes write exactly the same thing.

## Install

1. Download `AveeCare QuickBooks Connector_x.y.z_x64-setup.exe` from the
   [latest release](https://github.com/aveecare/quickbooks-aveecare-connector/releases/latest)
   onto the computer where QuickBooks Desktop is installed. (An `.msi` is also published for IT
   departments that deploy with Group Policy or Intune.)
2. Run it. Windows will probably say **"Windows protected your PC"** (see SmartScreen below).
   Choose **More info**, then **Run anyway**.
3. Open the Connector and sign in to AveeCare.

### SmartScreen: why Windows warns

The installers are **not code-signed** (AveeCare has no Authenticode certificate yet). Windows
SmartScreen warns about every unsigned download until it has seen enough people install it, and
some company-managed computers block unsigned installers outright. The warning is about the
missing publisher signature, not about anything found in the file.

To remove the warning, buy an OV or EV code-signing certificate (EV is trusted by SmartScreen at
once; OV earns trust over time) and add it to the release workflow: Tauri signs with
`bundle.windows.certificateThumbprint` or a `signCommand` (for example Azure Trusted Signing).
Updates are a separate matter and already protected (see Auto-update).

## First connection to QuickBooks

QuickBooks asks once, on the first sync, whether to let the Connector in:

1. Open QuickBooks Desktop and the agency's company file. Sign in to QuickBooks as the **Admin**
   user (only the Admin can authorize an application), in single-user mode for this first time.
2. In the Connector, press **Sync now**.
3. QuickBooks shows "An application is requesting access": **AveeCare Connector**. Choose
   **"Yes, always; allow access even if QuickBooks is not running"** or **"Yes, whenever this
   QuickBooks company file is open"**, then **Continue**, then **Done**.

You can review or remove the permission later in QuickBooks: Edit, Preferences, Integrated
Applications, Company Preferences.

### Run both programs the same way

Windows will not let a program that is running normally talk to QuickBooks if QuickBooks was
started with **Run as administrator** (and the reverse). Windows reports that as "Could not start
QuickBooks", which sounds as if QuickBooks were closed. The Connector checks for this first and
says so plainly, with a button to reopen itself the same way as QuickBooks.

## Using it

- **Sync now** sends everything the office queued in AveeCare (Payroll, Send to QuickBooks
  Desktop; Billing, send invoices) and shows live progress.
- After every sync the window shows what landed in QuickBooks (hours, invoices with totals,
  payments and, when the agency turned them on in AveeCare, mileage reimbursement checks) and, for anything QuickBooks refused, its reason and where to fix it in AveeCare.
  Refused items stay queued, so pressing Sync again after the fix sends them.
- **Activity** lists every sync with full details (company file, QuickBooks edition, every step).
- **Settings**: sync automatically (every 15 minutes to every 4 hours) while QuickBooks is open; start
  with Windows (opens quietly in the tray); sign out; check for updates.
- Closing the window keeps the Connector in the tray. Quit from the tray menu.

Guards: the Connector refuses to send into a different company file than the one AveeCare last
synced with (so a test file or another agency's file is never written to by accident) until the
person chooses to use the open file, and every send is tied to the person who signed in.

## How it works

```
AveeCare server                         Connector (this app)            QuickBooks Desktop
/api/qbwc/connector  <-- JSON/HTTPS -->  Rust sync loop  <-- COM -->     QBXMLRP2.RequestProcessor
  (Cognito JWT checked,                  carries qbXML                   OpenConnection2, BeginSession,
   company + role from the               both ways                       ProcessRequest, EndSession,
   server's own records)                                                 CloseConnection
```

The server endpoint drives the same session machinery as the Web Connector SOAP route
(`authenticate`, `sendRequestXML`, `receiveResponseXML`, `getLastError`, `closeConnection`), so the
Connector and the Web Connector can never disagree about what is sent.

| Path | What it does |
|---|---|
| `src-tauri/src/sync.rs` | The sync loop: open QuickBooks, ask AveeCare for the next request, carry it, report the response, close. |
| `src-tauri/src/qb/` | The QuickBooks transport behind the `QbTransport` trait: `com.rs` (Intuit's COM interface), `fake.rs` (tests), `emulator.rs` (testing only, loopback only), `process.rs` (is QuickBooks open, and elevated). |
| `src-tauri/src/auth.rs`, `srp.rs`, `session.rs` | AveeCare sign-in: Cognito SRP like the web app, authenticator-app codes, refresh token in Windows Credential Manager. |
| `src-tauri/src/api.rs` | The AveeCare endpoint client. |
| `src-tauri/src/lib.rs` | Window commands, tray, auto-sync, start with Windows, updater. |
| `src-tauri/src/bin/aveecare-qb-sync.rs` | Headless runner for the end-to-end test. |
| `ui/` | The window: plain HTML, CSS and JavaScript (no build step). |

## Develop

Requirements: Rust (stable, MSVC toolchain), the Tauri CLI (`cargo install tauri-cli --version "^2" --locked`),
WebView2 (present on Windows 10/11).

```powershell
cd src-tauri
cargo test --lib             # unit tests (QuickBooks and AveeCare are faked)
cargo tauri dev              # run the app
cargo tauri build            # installers in src-tauri/target/release/bundle/{nsis,msi}
```

Signed updater files need the updater key: set `TAURI_SIGNING_PRIVATE_KEY` to the key file's
contents (and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` to empty) before `cargo tauri build`.

To point a development copy at another AveeCare server, open Settings, Advanced (or the link
under the sign-in form) and enter the server address.

## Release

One command, from a clean `main`:

```powershell
pwsh -File scripts/release.ps1 -Version 0.2.0
```

It bumps the version, commits, stores the updater key as the `TAURI_SIGNING_PRIVATE_KEY`
repository secret the first time, and pushes the tag. The `release` workflow
(`.github/workflows/release.yml`) runs the unit tests, builds the NSIS and MSI installers, and
publishes the GitHub release with `latest.json`.

### Auto-update

Installed copies check
`https://github.com/aveecare/quickbooks-aveecare-connector/releases/latest/download/latest.json`
every time the Connector opens, and install a newer version before anything else (the window
shows the progress and the new version opens by itself). Offline, the Connector opens as it is
and checks again every few hours, never during a sync. Settings, Check for updates does the
same on demand. If an update cannot install, the window says why and offers Try again or Open
this version for now, so a bad release never locks an agency out. Every update
is verified against the public key in `src-tauri/tauri.conf.json`; a file not signed with the
matching private key is refused, so this protection does not depend on code signing.

The private key lives at `%USERPROFILE%\.tauri\aveecare-qb-connector.key` on the release machine
and in the repository secret. It is never committed. **Back it up**: if it is lost, installed
copies can no longer update themselves and must be reinstalled by hand after a new key is made.
