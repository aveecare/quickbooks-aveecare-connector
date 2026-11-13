//! Signing in to AveeCare: the same Cognito user pool and app client the web
//! app uses, spoken to directly over Cognito's JSON API.
//!
//! - Password sign-in is SRP (the password never leaves the machine). If the
//!   pool's client does not allow SRP but allows plain password sign-in (the
//!   fully local test stack), that is used instead.
//! - Two-step verification: an authenticator-app code (SOFTWARE_TOKEN_MFA).
//!   A login that has not set one up yet while the pool requires it is told to
//!   set it up in AveeCare first; we never enrol a factor from here.
//! - Staying signed in: the refresh token, kept in Windows Credential Manager.

use crate::config::USER_AGENT;
use crate::srp::{cognito_timestamp, SrpClient};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64URL, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CognitoConfig {
    pub region: String,
    pub user_pool_id: String,
    pub client_id: String,
    /// `https://cognito-idp.<region>.amazonaws.com/`, or the local stack's proxy.
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds when the ID token stops being accepted.
    pub expires_at: i64,
}

impl Tokens {
    pub fn seconds_left(&self, now: i64) -> i64 {
        self.expires_at - now
    }
}

#[derive(Debug, Clone)]
pub enum SignInStep {
    Done(Tokens),
    /// Enter the six-digit code from the authenticator app.
    TotpCode { session: String, username: String },
}

#[derive(Debug, Clone, thiserror::Error, Serialize)]
#[serde(tag = "code", content = "message")]
pub enum AuthError {
    #[error("{0}")]
    WrongPassword(String),
    #[error("{0}")]
    WrongCode(String),
    #[error("{0}")]
    NeedsWebSetup(String),
    #[error("{0}")]
    TooManyTries(String),
    #[error("{0}")]
    SignedOut(String),
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    Other(String),
}

/// The JSON claims of a JWT, unverified (the server verifies; we only read
/// the expiry and the person's name for display).
pub fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = B64URL.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn exp_of(token: &str) -> i64 {
    jwt_claims(token).and_then(|c| c.get("exp").and_then(|v| v.as_i64())).unwrap_or(0)
}

pub struct CognitoClient {
    cfg: CognitoConfig,
    http: reqwest::blocking::Client,
}

impl CognitoClient {
    pub fn new(cfg: CognitoConfig) -> Self {
        let http = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(30))
            .build()
            .expect("http client");
        Self { cfg, http }
    }

    pub fn config(&self) -> &CognitoConfig {
        &self.cfg
    }

    fn call(&self, target: &str, body: Value) -> Result<Value, AuthError> {
        let res = self
            .http
            .post(&self.cfg.endpoint)
            .header("Content-Type", "application/x-amz-json-1.1")
            .header("X-Amz-Target", format!("AWSCognitoIdentityProviderService.{target}"))
            .body(body.to_string())
            .send()
            .map_err(|e| AuthError::Network(network_sentence(&e)))?;
        let status = res.status();
        let text = res.text().map_err(|e| AuthError::Network(network_sentence(&e)))?;
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(v);
        }
        let kind = v.get("__type").and_then(|t| t.as_str()).unwrap_or("").rsplit('#').next().unwrap_or("").to_string();
        let message = v.get("message").or_else(|| v.get("Message")).and_then(|m| m.as_str()).unwrap_or("").to_string();
        Err(cognito_error(&kind, &message))
    }

    /// Start signing in. SRP first; the plain password flow only when the
    /// pool's client refuses SRP.
    pub fn sign_in(&self, email: &str, password: &str) -> Result<SignInStep, AuthError> {
        match self.sign_in_srp(email, password) {
            Err(AuthError::Other(m)) if m.contains("USER_SRP_AUTH") || m.contains("not enabled") || m.contains("not supported") => {
                self.sign_in_password(email, password)
            }
            other => other,
        }
    }

    fn sign_in_srp(&self, email: &str, password: &str) -> Result<SignInStep, AuthError> {
        let srp = SrpClient::new(&self.cfg.user_pool_id);
        let started = self.call(
            "InitiateAuth",
            json!({
                "AuthFlow": "USER_SRP_AUTH",
                "ClientId": self.cfg.client_id,
                "AuthParameters": { "USERNAME": email, "SRP_A": srp.srp_a() },
            }),
        )?;
        if started.get("ChallengeName").and_then(|c| c.as_str()) != Some("PASSWORD_VERIFIER") {
            return self.next_step(started, email);
        }
        let p = started.get("ChallengeParameters").cloned().unwrap_or(Value::Null);
        let get = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let user_id = get("USER_ID_FOR_SRP");
        let timestamp = cognito_timestamp(chrono::Utc::now());
        let signature = srp
            .password_claim(&user_id, password, &get("SALT"), &get("SRP_B"), &get("SECRET_BLOCK"), &timestamp)
            .map_err(AuthError::Other)?;
        let answered = self.call(
            "RespondToAuthChallenge",
            json!({
                "ChallengeName": "PASSWORD_VERIFIER",
                "ClientId": self.cfg.client_id,
                "ChallengeResponses": {
                    "USERNAME": user_id,
                    "PASSWORD_CLAIM_SECRET_BLOCK": get("SECRET_BLOCK"),
                    "PASSWORD_CLAIM_SIGNATURE": signature,
                    "TIMESTAMP": timestamp,
                },
                "Session": started.get("Session").cloned().unwrap_or(Value::Null),
            }),
        )?;
        self.next_step(answered, &user_id)
    }

    fn sign_in_password(&self, email: &str, password: &str) -> Result<SignInStep, AuthError> {
        let res = self.call(
            "InitiateAuth",
            json!({
                "AuthFlow": "USER_PASSWORD_AUTH",
                "ClientId": self.cfg.client_id,
                "AuthParameters": { "USERNAME": email, "PASSWORD": password },
            }),
        )?;
        self.next_step(res, email)
    }

    /// Read what Cognito asked for next.
    fn next_step(&self, v: Value, username: &str) -> Result<SignInStep, AuthError> {
        if let Some(result) = v.get("AuthenticationResult") {
            return tokens_from(result, None).map(SignInStep::Done);
        }
        let session = v.get("Session").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let challenge_user = v
            .get("ChallengeParameters")
            .and_then(|p| p.get("USER_ID_FOR_SRP").or_else(|| p.get("USERNAME")))
            .and_then(|u| u.as_str())
            .unwrap_or(username)
            .to_string();
        match v.get("ChallengeName").and_then(|c| c.as_str()).unwrap_or("") {
            "SOFTWARE_TOKEN_MFA" => Ok(SignInStep::TotpCode { session, username: challenge_user }),
            // Both methods are on for this login: Cognito wants the method chosen
            // first, and only then asks for the code.
            "SELECT_MFA_TYPE" => {
                let choices = v
                    .get("ChallengeParameters")
                    .and_then(|p| p.get("MFAS_CAN_CHOOSE"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                if !choices.contains("SOFTWARE_TOKEN_MFA") {
                    return Err(AuthError::NeedsWebSetup(
                        "This login uses text-message codes, which the Connector does not support. Turn on an authenticator app in AveeCare, then sign in here.".into(),
                    ));
                }
                let res = self.call(
                    "RespondToAuthChallenge",
                    json!({
                        "ChallengeName": "SELECT_MFA_TYPE",
                        "ClientId": self.cfg.client_id,
                        "Session": session,
                        "ChallengeResponses": { "USERNAME": challenge_user, "ANSWER": "SOFTWARE_TOKEN_MFA" },
                    }),
                )?;
                self.next_step(res, username)
            }
            "MFA_SETUP" => Err(AuthError::NeedsWebSetup(
                "Your agency requires two-step verification. Set it up by signing in to AveeCare in your browser first, then sign in here.".into(),
            )),
            "NEW_PASSWORD_REQUIRED" => Err(AuthError::NeedsWebSetup(
                "AveeCare needs you to choose a new password. Sign in to AveeCare in your browser to do that, then sign in here.".into(),
            )),
            "SMS_MFA" => Err(AuthError::NeedsWebSetup(
                "This login uses text-message codes, which the Connector does not support. Turn on an authenticator app in AveeCare, then sign in here.".into(),
            )),
            other => Err(AuthError::Other(format!("AveeCare asked for a sign-in step the Connector does not know ({other})."))),
        }
    }

    /// Answer the authenticator-app challenge.
    pub fn confirm_totp(&self, session: &str, username: &str, code: &str) -> Result<Tokens, AuthError> {
        let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
        if code.len() != 6 {
            return Err(AuthError::WrongCode("Enter the six-digit code from your authenticator app.".into()));
        }
        let res = self.call(
            "RespondToAuthChallenge",
            json!({
                "ChallengeName": "SOFTWARE_TOKEN_MFA",
                "ClientId": self.cfg.client_id,
                "Session": session,
                "ChallengeResponses": { "USERNAME": username, "SOFTWARE_TOKEN_MFA_CODE": code },
            }),
        )?;
        match self.next_step(res, username)? {
            SignInStep::Done(t) => Ok(t),
            SignInStep::TotpCode { .. } => Err(AuthError::WrongCode("That code was not accepted. Try the newest code.".into())),
        }
    }

    /// New ID and access tokens from the refresh token. The current Cognito
    /// call first (it is the only one allowed once refresh-token rotation is
    /// on); the older InitiateAuth flow where it is not available.
    pub fn refresh(&self, refresh_token: &str) -> Result<Tokens, AuthError> {
        let modern = self.call(
            "GetTokensFromRefreshToken",
            json!({ "ClientId": self.cfg.client_id, "RefreshToken": refresh_token }),
        );
        let v = match modern {
            Ok(v) => v,
            Err(AuthError::SignedOut(m)) => return Err(AuthError::SignedOut(m)),
            Err(AuthError::Network(m)) => return Err(AuthError::Network(m)),
            Err(_) => self.call(
                "InitiateAuth",
                json!({
                    "AuthFlow": "REFRESH_TOKEN_AUTH",
                    "ClientId": self.cfg.client_id,
                    "AuthParameters": { "REFRESH_TOKEN": refresh_token },
                }),
            )?,
        };
        let result = v
            .get("AuthenticationResult")
            .ok_or_else(|| AuthError::SignedOut("Your AveeCare sign-in has ended. Sign in again.".into()))?;
        tokens_from(result, Some(refresh_token))
    }

    /// Best effort: end the refresh token so a copy of it is worthless.
    pub fn revoke(&self, refresh_token: &str) {
        let _ = self.call("RevokeToken", json!({ "ClientId": self.cfg.client_id, "Token": refresh_token }));
    }
}

fn tokens_from(result: &Value, previous_refresh: Option<&str>) -> Result<Tokens, AuthError> {
    let s = |k: &str| result.get(k).and_then(|v| v.as_str()).map(|v| v.to_string());
    let id_token = s("IdToken").ok_or_else(|| AuthError::Other("AveeCare did not return a sign-in token.".into()))?;
    let access_token = s("AccessToken").unwrap_or_default();
    let refresh_token = s("RefreshToken").or_else(|| previous_refresh.map(|r| r.to_string()));
    let expires_at = exp_of(&id_token);
    Ok(Tokens { id_token, access_token, refresh_token, expires_at })
}

fn network_sentence(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "AveeCare sign-in did not answer in time. Check your internet connection and try again.".into()
    } else {
        "Could not reach AveeCare sign-in. Check your internet connection and try again.".into()
    }
}

/// Cognito's exception names, in words.
pub fn cognito_error(kind: &str, message: &str) -> AuthError {
    match kind {
        "NotAuthorizedException" if message.to_lowercase().contains("refresh token") => {
            AuthError::SignedOut("Your AveeCare sign-in has ended. Sign in again.".into())
        }
        "NotAuthorizedException" if message.to_lowercase().contains("disabled") => {
            AuthError::WrongPassword("This AveeCare login has been turned off. Ask your office to turn it back on.".into())
        }
        "NotAuthorizedException" if message.to_lowercase().contains("password attempts exceeded") => {
            AuthError::TooManyTries("Too many sign-in attempts. Wait a few minutes, then try again.".into())
        }
        "NotAuthorizedException" | "UserNotFoundException" => {
            if message.to_lowercase().contains("session") {
                AuthError::WrongCode("That sign-in took too long. Start again.".into())
            } else {
                AuthError::WrongPassword("That email and password do not match an AveeCare login.".into())
            }
        }
        "CodeMismatchException" => AuthError::WrongCode("That code is not right. Try the newest code from your authenticator app.".into()),
        "ExpiredCodeException" => AuthError::WrongCode("That code has expired. Enter the newest one.".into()),
        "EnableSoftwareTokenMFAException" => AuthError::WrongCode("That code is not right. Try the newest code from your authenticator app.".into()),
        "PasswordResetRequiredException" => AuthError::NeedsWebSetup("Your password needs to be reset. Reset it on the AveeCare sign-in page, then sign in here.".into()),
        "UserNotConfirmedException" => AuthError::NeedsWebSetup("This login is not confirmed yet. Finish setting it up in AveeCare first.".into()),
        "TooManyRequestsException" | "LimitExceededException" | "TooManyFailedAttemptsException" => {
            AuthError::TooManyTries("Too many tries. Wait a minute, then try again.".into())
        }
        "" => AuthError::Other(if message.is_empty() { "AveeCare sign-in did not work. Try again.".into() } else { message.to_string() }),
        _ => AuthError::Other(if message.is_empty() { format!("AveeCare sign-in did not work ({kind}).") } else { message.to_string() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_jwt_expiry_and_claims() {
        let payload = B64URL.encode(br#"{"exp":1900000000,"email":"a@b.co"}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(exp_of(&token), 1900000000);
        assert_eq!(jwt_claims(&token).unwrap()["email"], "a@b.co");
        assert_eq!(exp_of("garbage"), 0);
    }

    #[test]
    fn maps_cognito_errors_to_words() {
        assert!(matches!(cognito_error("NotAuthorizedException", "Incorrect username or password."), AuthError::WrongPassword(_)));
        assert!(matches!(cognito_error("NotAuthorizedException", "Refresh Token has expired"), AuthError::SignedOut(_)));
        assert!(matches!(cognito_error("NotAuthorizedException", "Password attempts exceeded"), AuthError::TooManyTries(_)));
        assert!(matches!(cognito_error("CodeMismatchException", "Invalid code"), AuthError::WrongCode(_)));
        assert!(matches!(cognito_error("PasswordResetRequiredException", ""), AuthError::NeedsWebSetup(_)));
        assert!(matches!(cognito_error("InvalidParameterException", "USER_SRP_AUTH is not enabled for the client."), AuthError::Other(m) if m.contains("USER_SRP_AUTH")));
    }

    #[test]
    fn tokens_keep_the_old_refresh_token_when_none_is_returned() {
        let payload = B64URL.encode(br#"{"exp":1900000000}"#);
        let id = format!("h.{payload}.s");
        let t = tokens_from(&json!({ "IdToken": id, "AccessToken": "a" }), Some("r1")).unwrap();
        assert_eq!(t.refresh_token.as_deref(), Some("r1"));
        assert_eq!(t.expires_at, 1900000000);
        let t = tokens_from(&json!({ "IdToken": id, "AccessToken": "a", "RefreshToken": "r2" }), Some("r1")).unwrap();
        assert_eq!(t.refresh_token.as_deref(), Some("r2"));
    }

    // ---- A scripted Cognito on a loopback port ---------------------------
    //
    // The production pool REQUIRES an authenticator code (amplify_outputs.json
    // mfa_configuration REQUIRED, TOTP), and the local stack's Cognito never
    // issues that challenge, so these tests are the proof of the code path.

    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::{Arc, Mutex};

    type Script = Arc<dyn Fn(&str, &Value) -> Value + Send + Sync>;

    fn fake_cognito(script: Script) -> (CognitoClient, Arc<Mutex<Vec<(String, Value)>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let (mut target, mut len) = (String::new(), 0usize);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    if lower.starts_with("x-amz-target:") {
                        target = line.trim().rsplit('.').next().unwrap_or("").to_string();
                    }
                }
                let mut body = vec![0u8; len];
                reader.read_exact(&mut body).unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let reply = script(&target, &body);
                log.lock().unwrap().push((target, body));
                let (status, text) = match reply.get("__type") {
                    Some(_) => ("400 Bad Request", reply.to_string()),
                    None => ("200 OK", reply.to_string()),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/x-amz-json-1.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
            }
        });
        let cfg = CognitoConfig {
            region: "us-east-2".into(),
            user_pool_id: "us-east-2_TestPool".into(),
            client_id: "client123".into(),
            endpoint: format!("http://127.0.0.1:{port}"),
        };
        (CognitoClient::new(cfg), seen)
    }

    fn id_token() -> String {
        format!("h.{}.s", B64URL.encode(br#"{"exp":1900000000,"email":"office@agency.test"}"#))
    }

    /// SRP's first two legs, answered the way Cognito answers them; `after` is
    /// what Cognito says once the password proof is accepted.
    fn srp_then(after: Value) -> impl Fn(&str, &Value) -> Option<Value> {
        move |target, body| match (target, body["ChallengeName"].as_str()) {
            ("InitiateAuth", _) => Some(json!({
                "ChallengeName": "PASSWORD_VERIFIER",
                "ChallengeParameters": {
                    "USER_ID_FOR_SRP": "uuid-office",
                    "SALT": "1a2b3c4d",
                    "SRP_B": "abcdef0123456789abcdef0123456789abcdef0123456789",
                    "SECRET_BLOCK": "c2VjcmV0LWJsb2Nr",
                },
                "Session": "s-verifier",
            })),
            ("RespondToAuthChallenge", Some("PASSWORD_VERIFIER")) => Some(after.clone()),
            _ => None,
        }
    }

    #[test]
    fn authenticator_code_completes_sign_in() {
        let srp = srp_then(json!({
            "ChallengeName": "SOFTWARE_TOKEN_MFA",
            "ChallengeParameters": { "USER_ID_FOR_SRP": "uuid-office" },
            "Session": "s-mfa",
        }));
        let id = id_token();
        let (client, seen) = fake_cognito(Arc::new(move |target, body| {
            if let Some(v) = srp(target, body) {
                return v;
            }
            if body["ChallengeName"] == "SOFTWARE_TOKEN_MFA" && body["ChallengeResponses"]["SOFTWARE_TOKEN_MFA_CODE"] == "123456" {
                return json!({ "AuthenticationResult": { "IdToken": id, "AccessToken": "a", "RefreshToken": "r" } });
            }
            json!({ "__type": "CodeMismatchException", "message": "Invalid code received for user" })
        }));
        let step = client.sign_in("office@agency.test", "Correct-Horse-1").unwrap();
        let SignInStep::TotpCode { session, username } = step else { panic!("expected the code step") };
        assert_eq!((session.as_str(), username.as_str()), ("s-mfa", "uuid-office"));

        // A wrong code is a sentence, not a crash, and the right one signs in.
        assert!(matches!(client.confirm_totp(&session, &username, "999 999"), Err(AuthError::WrongCode(_))));
        assert!(matches!(client.confirm_totp(&session, &username, "12345"), Err(AuthError::WrongCode(_))));
        let t = client.confirm_totp(&session, &username, "123 456").unwrap();
        assert_eq!(t.refresh_token.as_deref(), Some("r"));
        assert_eq!(t.expires_at, 1900000000);

        let seen = seen.lock().unwrap();
        let mfa = seen.iter().rev().find(|(_, b)| b["ChallengeName"] == "SOFTWARE_TOKEN_MFA").unwrap();
        assert_eq!(mfa.1["Session"], "s-mfa");
        assert_eq!(mfa.1["ChallengeResponses"]["USERNAME"], "uuid-office");
        assert_eq!(seen[0].1["AuthFlow"], "USER_SRP_AUTH", "signs in with SRP, like the web app");
    }

    #[test]
    fn a_login_with_both_methods_chooses_the_authenticator_first() {
        let srp = srp_then(json!({
            "ChallengeName": "SELECT_MFA_TYPE",
            "ChallengeParameters": { "USER_ID_FOR_SRP": "uuid-office", "MFAS_CAN_CHOOSE": "[\"SMS_MFA\",\"SOFTWARE_TOKEN_MFA\"]" },
            "Session": "s-select",
        }));
        let (client, seen) = fake_cognito(Arc::new(move |target, body| {
            if let Some(v) = srp(target, body) {
                return v;
            }
            if body["ChallengeName"] == "SELECT_MFA_TYPE" && body["ChallengeResponses"]["ANSWER"] == "SOFTWARE_TOKEN_MFA" && body["Session"] == "s-select" {
                return json!({ "ChallengeName": "SOFTWARE_TOKEN_MFA", "ChallengeParameters": {}, "Session": "s-mfa2" });
            }
            json!({ "__type": "InvalidParameterException", "message": "unexpected" })
        }));
        let step = client.sign_in("office@agency.test", "Correct-Horse-1").unwrap();
        let SignInStep::TotpCode { session, .. } = step else { panic!("expected the code step") };
        assert_eq!(session, "s-mfa2", "the code is answered on the session that asked for it");
        assert!(seen.lock().unwrap().iter().any(|(_, b)| b["ChallengeName"] == "SELECT_MFA_TYPE"));
    }

    #[test]
    fn mfa_the_connector_cannot_finish_sends_the_person_to_the_web() {
        for (challenge, params) in [
            ("MFA_SETUP", json!({})),
            ("SMS_MFA", json!({})),
            ("SELECT_MFA_TYPE", json!({ "MFAS_CAN_CHOOSE": "[\"SMS_MFA\"]" })),
            ("NEW_PASSWORD_REQUIRED", json!({})),
        ] {
            let srp = srp_then(json!({ "ChallengeName": challenge, "ChallengeParameters": params, "Session": "s" }));
            let (client, _) = fake_cognito(Arc::new(move |t, b| srp(t, b).unwrap_or(json!({ "__type": "X", "message": "no" }))));
            let got = client.sign_in("office@agency.test", "Correct-Horse-1");
            assert!(matches!(got, Err(AuthError::NeedsWebSetup(_))), "{challenge}: {got:?}");
        }
    }
}
