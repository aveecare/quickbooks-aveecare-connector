//! Fixed facts about the app.

/// Where AveeCare lives. Changeable in Settings → Advanced for testing only.
pub const DEFAULT_SERVER: &str = "https://app.aveecare.com";

/// The name QuickBooks shows in its "an application is requesting access"
/// dialog and lists under Edit → Preferences → Integrated Applications.
pub const QB_APP_NAME: &str = "AveeCare Connector";

/// QuickBooks keys an integrated application's permission on this id plus the
/// name. Empty is what Intuit's own samples use for a desktop app.
pub const QB_APP_ID: &str = "";

/// Sent on every request. The production WAF blocks requests with no
/// User-Agent (`NoUserAgent_HEADER`), so this is load-bearing, not cosmetic.
pub const USER_AGENT: &str = concat!("AveeCareQuickBooksConnector/", env!("CARGO_PKG_VERSION"), " (Windows)");

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The JSON protocol this build speaks with `/api/qbwc/connector`.
pub const PROTOCOL_VERSION: u32 = 1;

/// The service name the refresh token is filed under in Windows Credential Manager.
pub const KEYRING_SERVICE: &str = "AveeCare QuickBooks Connector";

/// The qbXML version to ask HostQuery in before we know what the file speaks.
/// 13.0 is understood by every QuickBooks Desktop still in support.
pub const PROBE_QBXML_VERSION: &str = "13.0";

/// The emulator transport is honored only for a loopback address, so no
/// setting can send an agency's QuickBooks traffic anywhere else.
pub const EMULATOR_ENV: &str = "AVEECARE_QB_EMULATOR_URL";
