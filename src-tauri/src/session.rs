//! Who is signed in, and a current ID token for every request.
//!
//! The refresh token is kept in Windows Credential Manager under the server's
//! address, so restarting the app (or Windows) does not sign the person out,
//! and nothing secret sits in a plain file.

use crate::api::{ApiError, ConnectorApi, HttpConnectorApi, Profile};
use crate::auth::{jwt_claims, AuthError, CognitoClient, CognitoConfig, SignInStep, Tokens};
use crate::config::KEYRING_SERVICE;
use std::sync::Mutex;

/// Refresh when the ID token has less than this many seconds left. A long
/// sync asks for a token before every request, so it never runs on a stale one.
const REFRESH_MARGIN_SECONDS: i64 = 300;

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, value: &str);
    fn delete(&self, key: &str);
}

/// Windows Credential Manager.
pub struct KeyringStore;

impl SecretStore for KeyringStore {
    fn get(&self, key: &str) -> Option<String> {
        keyring::Entry::new(KEYRING_SERVICE, key).ok()?.get_password().ok()
    }
    fn set(&self, key: &str, value: &str) {
        if let Ok(e) = keyring::Entry::new(KEYRING_SERVICE, key) {
            let _ = e.set_password(value);
        }
    }
    fn delete(&self, key: &str) {
        if let Ok(e) = keyring::Entry::new(KEYRING_SERVICE, key) {
            let _ = e.delete_credential();
        }
    }
}

/// In memory only: the headless test runner, which must never touch the
/// person's Credential Manager.
#[derive(Default)]
pub struct MemorySecrets(Mutex<std::collections::HashMap<String, String>>);

impl SecretStore for MemorySecrets {
    fn get(&self, k: &str) -> Option<String> {
        self.0.lock().unwrap().get(k).cloned()
    }
    fn set(&self, k: &str, v: &str) {
        self.0.lock().unwrap().insert(k.into(), v.into());
    }
    fn delete(&self, k: &str) {
        self.0.lock().unwrap().remove(k);
    }
}

pub struct Session {
    server_url: String,
    cognito: Mutex<Option<CognitoClient>>,
    tokens: Mutex<Option<Tokens>>,
    graphql: Mutex<Option<String>>,
    secrets: Box<dyn SecretStore>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Session {
    pub fn new(server_url: &str, secrets: Box<dyn SecretStore>) -> Self {
        Self { server_url: server_url.trim_end_matches('/').to_string(), cognito: Mutex::new(None), tokens: Mutex::new(None), graphql: Mutex::new(None), secrets }
    }

    pub fn server_url(&self) -> &str {
        &self.server_url
    }

    pub fn api(&self) -> HttpConnectorApi {
        HttpConnectorApi::new(&self.server_url)
    }

    fn secret_key(&self) -> String {
        format!("refresh:{}", self.server_url)
    }

    /// The pool and app client, from the server. Cached once read.
    fn cognito(&self) -> Result<CognitoClient, AuthError> {
        if let Some(c) = self.cognito.lock().unwrap().as_ref() {
            return Ok(CognitoClient::new(c.config().clone()));
        }
        let info = self.api().server_info().map_err(|e| match e {
            ApiError::Network(m) => AuthError::Network(m),
            other => AuthError::Other(other.to_string()),
        })?;
        let cfg: CognitoConfig = info.cognito.ok_or_else(|| AuthError::Other("This AveeCare server does not offer sign-in for the Connector.".into()))?;
        *self.cognito.lock().unwrap() = Some(CognitoClient::new(cfg.clone()));
        Ok(CognitoClient::new(cfg))
    }

    fn keep(&self, t: Tokens) {
        if let Some(r) = &t.refresh_token {
            self.secrets.set(&self.secret_key(), r);
        }
        *self.tokens.lock().unwrap() = Some(t);
    }

    pub fn sign_in(&self, email: &str, password: &str) -> Result<SignInStep, AuthError> {
        let step = self.cognito()?.sign_in(email.trim(), password)?;
        if let SignInStep::Done(t) = &step {
            self.keep(t.clone());
        }
        Ok(step)
    }

    pub fn confirm_code(&self, session: &str, username: &str, code: &str) -> Result<(), AuthError> {
        let t = self.cognito()?.confirm_totp(session, username, code)?;
        self.keep(t);
        Ok(())
    }

    /// Is there a sign-in to resume (a stored refresh token)?
    pub fn has_saved_sign_in(&self) -> bool {
        self.tokens.lock().unwrap().is_some() || self.secrets.get(&self.secret_key()).is_some()
    }

    /// A current ID token: the one in hand, or a fresh one from the refresh token.
    fn fresh_tokens(&self) -> Result<Tokens, ApiError> {
        if let Some(t) = self.tokens.lock().unwrap().as_ref() {
            if t.seconds_left(now()) > REFRESH_MARGIN_SECONDS {
                return Ok(t.clone());
            }
        }
        let refresh = self
            .tokens
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|t| t.refresh_token.clone())
            .or_else(|| self.secrets.get(&self.secret_key()))
            .ok_or_else(|| ApiError::SignedOut("You are signed out. Sign in to AveeCare.".into()))?;
        let cognito = self.cognito().map_err(|e| match e {
            AuthError::Network(m) => ApiError::Network(m),
            other => ApiError::Server(other.to_string()),
        })?;
        match cognito.refresh(&refresh) {
            Ok(t) => {
                self.keep(t.clone());
                Ok(t)
            }
            Err(AuthError::Network(m)) => Err(ApiError::Network(m)),
            Err(e) => {
                // The refresh token is no longer accepted: forget it.
                self.secrets.delete(&self.secret_key());
                *self.tokens.lock().unwrap() = None;
                Err(ApiError::SignedOut(match e {
                    AuthError::SignedOut(m) => m,
                    _ => "Your AveeCare sign-in has ended. Sign in again.".into(),
                }))
            }
        }
    }

    /// The ID token: what `/api/qbwc/connector` checks.
    pub fn id_token(&self) -> Result<String, ApiError> {
        Ok(self.fresh_tokens()?.id_token)
    }

    /// The access token: what the AveeCare API (AppSync) checks, the same token
    /// the web app sends. It carries the agency and role claims the server's
    /// own checks read (the pre-token trigger writes them to the access token).
    pub fn access_token(&self) -> Result<String, ApiError> {
        let t = self.fresh_tokens()?;
        if t.access_token.is_empty() {
            return Err(ApiError::SignedOut("Your AveeCare sign-in has ended. Sign in again.".into()));
        }
        Ok(t.access_token)
    }

    /// The AveeCare API address, from the server's own description of itself.
    pub fn graphql_url(&self) -> Result<String, ApiError> {
        if let Some(u) = self.graphql.lock().unwrap().as_ref() {
            return Ok(u.clone());
        }
        let info = self.api().server_info()?;
        let url = info.api.map(|a| a.graphql).filter(|u| !u.is_empty()).ok_or_else(|| {
            ApiError::Server("This AveeCare server does not offer payroll and billing to the Connector yet. Update AveeCare, or send from AveeCare in your browser.".into())
        })?;
        *self.graphql.lock().unwrap() = Some(url.clone());
        Ok(url)
    }

    /// One AveeCare operation (billingAPI or payrollAPI), run with this sign-in.
    pub fn aveecare(&self, api: crate::aveecare::Api, operation: &str, input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
        let url = self.graphql_url()?;
        let token = self.access_token()?;
        crate::aveecare::AveeCareApi::new(&url).call(&token, api, operation, input)
    }

    /// Who is signed in, and may they connect (the server decides).
    pub fn profile(&self) -> Result<Profile, ApiError> {
        let token = self.id_token()?;
        self.api().profile(&token)
    }

    /// The signed-in email, read from the token (display only).
    pub fn email(&self) -> Option<String> {
        let t = self.tokens.lock().unwrap();
        jwt_claims(&t.as_ref()?.id_token)?.get("email")?.as_str().map(String::from)
    }

    pub fn sign_out(&self) {
        let refresh = self.tokens.lock().unwrap().take().and_then(|t| t.refresh_token).or_else(|| self.secrets.get(&self.secret_key()));
        if let (Some(r), Ok(c)) = (refresh, self.cognito()) {
            c.revoke(&r);
        }
        self.secrets.delete(&self.secret_key());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    type MemStore = MemorySecrets;

    #[test]
    fn a_fresh_token_is_used_without_refreshing() {
        let s = Session::new("http://127.0.0.1:1/", Box::new(MemStore::default()));
        s.keep(Tokens { id_token: "id".into(), access_token: "a".into(), refresh_token: Some("r".into()), expires_at: now() + 3600 });
        assert_eq!(s.id_token().unwrap(), "id");
        assert_eq!(s.access_token().unwrap(), "a");
        assert!(s.has_saved_sign_in());
        assert_eq!(s.secrets.get("refresh:http://127.0.0.1:1").as_deref(), Some("r"));
    }

    #[test]
    fn no_sign_in_is_signed_out() {
        let s = Session::new("http://127.0.0.1:1", Box::new(MemStore::default()));
        assert!(!s.has_saved_sign_in());
        assert!(matches!(s.id_token(), Err(ApiError::SignedOut(_))));
    }
}
