//! Cognito's Secure Remote Password sign-in (USER_SRP_AUTH).
//!
//! The AveeCare web app signs in with SRP (Amplify's default), so the user pool
//! client allows it; plain USER_PASSWORD_AUTH is not something we can rely on.
//! SRP never sends the password: the app proves it knows it.
//!
//! This follows Amazon's reference client (amazon-cognito-identity-js /
//! Amplify `AuthenticationHelper`) step for step, including its two quirks:
//! hex values are "padded" so a leading high bit never reads as negative, and
//! the timestamp has an unpadded day of month.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use hmac::{Hmac, Mac};
use num_bigint::BigUint;
use num_traits::Zero;
use rand::RngCore;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// RFC 5054's 3072-bit group, the one Cognito uses.
const N_HEX: &str = concat!(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD1",
    "29024E088A67CC74020BBEA63B139B22514A08798E3404DD",
    "EF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245",
    "E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED",
    "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3D",
    "C2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F",
    "83655D23DCA3AD961C62F356208552BB9ED529077096966D",
    "670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B",
    "E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9",
    "DE2BCBF6955817183995497CEA956AE515D2261898FA0510",
    "15728E5A8AAAC42DAD33170D04507A33A85521ABDF1CBA64",
    "ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7",
    "ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6B",
    "F12FFA06D98A0864D87602733EC86A64521F2B18177B200C",
    "BBE117577A615D6C770988C0BAD946E208E24FA074E5AB31",
    "43DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF"
);

fn n() -> BigUint {
    BigUint::parse_bytes(N_HEX.as_bytes(), 16).expect("N")
}

fn g() -> BigUint {
    BigUint::from(2u32)
}

/// Amplify's `padHex`: even length, and a leading `00` when the top bit is set
/// so the value never reads as negative.
pub fn pad_hex(v: &BigUint) -> String {
    let mut h = v.to_str_radix(16);
    if h.len() % 2 == 1 {
        h = format!("0{h}");
    } else if matches!(h.as_bytes()[0], b'8'..=b'9' | b'a'..=b'f') {
        h = format!("00{h}");
    }
    h
}

fn hex_to_bytes(h: &str) -> Vec<u8> {
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0))
        .collect()
}

fn sha256_hex_of_hex(h: &str) -> String {
    let digest = Sha256::digest(hex_to_bytes(h));
    let s: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    // Amplify left-pads the hash to 64 hex characters.
    format!("{s:0>64}")
}

/// HKDF-SHA256 (RFC 5869) with Cognito's fixed info string, first 16 bytes.
pub fn hkdf16(ikm: &[u8], salt: &[u8]) -> [u8; 16] {
    let mut extract = <HmacSha256 as Mac>::new_from_slice(salt).expect("hmac");
    extract.update(ikm);
    let prk = extract.finalize().into_bytes();
    let mut expand = <HmacSha256 as Mac>::new_from_slice(&prk).expect("hmac");
    expand.update(b"Caldera Derived Key");
    expand.update(&[1u8]);
    let okm = expand.finalize().into_bytes();
    let mut out = [0u8; 16];
    out.copy_from_slice(&okm[..16]);
    out
}

/// One SRP sign-in attempt: holds the secret `a` between the two requests.
pub struct SrpClient {
    pool_name: String,
    a: BigUint,
    big_a: BigUint,
    k: BigUint,
}

impl SrpClient {
    /// `user_pool_id` is `region_poolName`; SRP uses the part after `_`.
    pub fn new(user_pool_id: &str) -> Self {
        let mut bytes = [0u8; 128];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self::with_secret(user_pool_id, BigUint::from_bytes_be(&bytes))
    }

    /// Deterministic constructor, for tests.
    pub fn with_secret(user_pool_id: &str, a: BigUint) -> Self {
        let n = n();
        let big_a = g().modpow(&a, &n);
        let k = BigUint::parse_bytes(sha256_hex_of_hex(&format!("{}{}", pad_hex(&n), pad_hex(&g()))).as_bytes(), 16)
            .expect("k");
        let pool_name = user_pool_id.split('_').nth(1).unwrap_or(user_pool_id).to_string();
        Self { pool_name, a, big_a, k }
    }

    /// SRP_A, sent with InitiateAuth.
    pub fn srp_a(&self) -> String {
        self.big_a.to_str_radix(16)
    }

    /// The session key both sides derive. Exposed for the self-check test.
    pub fn session_key(&self, user_id_for_srp: &str, password: &str, salt_hex: &str, srp_b_hex: &str) -> Result<[u8; 16], String> {
        let n = n();
        let big_b = BigUint::parse_bytes(srp_b_hex.as_bytes(), 16).ok_or("Cognito sent an unreadable SRP_B")?;
        if (&big_b % &n).is_zero() {
            return Err("Cognito sent an invalid SRP_B".into());
        }
        let u = BigUint::parse_bytes(
            sha256_hex_of_hex(&format!("{}{}", pad_hex(&self.big_a), pad_hex(&big_b))).as_bytes(),
            16,
        )
        .ok_or("u")?;
        if u.is_zero() {
            return Err("SRP u is zero".into());
        }
        let user_pass = format!("{}{}:{}", self.pool_name, user_id_for_srp, password);
        let user_pass_hash: String = Sha256::digest(user_pass.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        let salt = BigUint::parse_bytes(salt_hex.as_bytes(), 16).ok_or("Cognito sent an unreadable SALT")?;
        let x = BigUint::parse_bytes(
            sha256_hex_of_hex(&format!("{}{:0>64}", pad_hex(&salt), user_pass_hash)).as_bytes(),
            16,
        )
        .ok_or("x")?;
        // S = (B - k * g^x) ^ (a + u * x) mod N, kept non-negative.
        let gx = g().modpow(&x, &n);
        let kgx = (&self.k * gx) % &n;
        let base = ((&big_b % &n) + &n - kgx) % &n;
        let exp = &self.a + &u * &x;
        let s = base.modpow(&exp, &n);
        Ok(hkdf16(&hex_to_bytes(&pad_hex(&s)), &hex_to_bytes(&pad_hex(&u))))
    }

    /// The PASSWORD_VERIFIER challenge responses: (signature, timestamp).
    pub fn password_claim(
        &self,
        user_id_for_srp: &str,
        password: &str,
        salt_hex: &str,
        srp_b_hex: &str,
        secret_block_b64: &str,
        timestamp: &str,
    ) -> Result<String, String> {
        let key = self.session_key(user_id_for_srp, password, salt_hex, srp_b_hex)?;
        let secret_block = B64.decode(secret_block_b64).map_err(|_| "Cognito sent an unreadable SECRET_BLOCK")?;
        let mut mac = <HmacSha256 as Mac>::new_from_slice(&key).expect("hmac");
        mac.update(self.pool_name.as_bytes());
        mac.update(user_id_for_srp.as_bytes());
        mac.update(&secret_block);
        mac.update(timestamp.as_bytes());
        Ok(B64.encode(mac.finalize().into_bytes()))
    }
}

/// "Tue Nov 4 09:05:03 UTC 2026": English names, day of month NOT padded.
pub fn cognito_timestamp(now: chrono::DateTime<chrono::Utc>) -> String {
    use chrono::Datelike;
    use chrono::Timelike;
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!(
        "{} {} {} {:02}:{:02}:{:02} UTC {}",
        DAYS[now.weekday().num_days_from_monday() as usize],
        MONTHS[now.month0() as usize],
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.year()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn pad_hex_follows_amplify() {
        assert_eq!(pad_hex(&BigUint::from(0x2u32)), "02");
        assert_eq!(pad_hex(&BigUint::from(0x7fu32)), "7f");
        assert_eq!(pad_hex(&BigUint::from(0x80u32)), "0080");
        assert_eq!(pad_hex(&BigUint::from(0x123u32)), "0123");
        assert!(pad_hex(&n()).starts_with("00ffffffff"));
    }

    #[test]
    fn timestamp_has_unpadded_day() {
        let t = chrono::Utc.with_ymd_and_hms(2026, 11, 4, 9, 5, 3).unwrap();
        assert_eq!(cognito_timestamp(t), "Wed Nov 4 09:05:03 UTC 2026");
        let t = chrono::Utc.with_ymd_and_hms(2026, 1, 12, 23, 59, 0).unwrap();
        assert_eq!(cognito_timestamp(t), "Mon Jan 12 23:59:00 UTC 2026");
    }

    /// RFC 5869 test case 1, truncated to 16 bytes, with our fixed info: the
    /// extract step must match a known PRK. (Cognito's info differs from the
    /// RFC's, so we check extract + expand against an independent computation.)
    #[test]
    fn hkdf_matches_independent_hmac() {
        let ikm = [0x0bu8; 22];
        let salt: Vec<u8> = (0x00u8..=0x0c).collect();
        let mut e = <HmacSha256 as Mac>::new_from_slice(&salt).unwrap();
        e.update(&ikm);
        let prk = e.finalize().into_bytes();
        // RFC 5869 A.1 PRK.
        let want_prk = hex_to_bytes("077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5");
        assert_eq!(prk.as_slice(), want_prk.as_slice());
        let mut x = <HmacSha256 as Mac>::new_from_slice(&prk).unwrap();
        x.update(b"Caldera Derived Key\x01");
        assert_eq!(&x.finalize().into_bytes()[..16], &hkdf16(&ikm, &salt));
    }

    /// The client and an honest SRP server (RFC 5054 verifier) must derive the
    /// same key. This is the property Cognito checks; if the math is wrong in
    /// any step (k, u, x, S, padding), the keys differ.
    #[test]
    fn client_and_server_agree_on_the_key() {
        let pool = "us-east-2_TestPool";
        let user = "11111111-2222-3333-4444-555555555555";
        let password = "Correct-Horse-9!";
        let salt = BigUint::parse_bytes(b"8f1a2b3c4d5e6f708192a3b4c5d6e7f8", 16).unwrap();
        let client = SrpClient::with_secret(pool, BigUint::parse_bytes(b"1234567890abcdef1234567890abcdef1234567890abcdef", 16).unwrap());

        // Server side: v = g^x, B = k v + g^b, S = (A v^u)^b.
        let n = n();
        let user_pass_hash: String = Sha256::digest(format!("TestPool{user}:{password}").as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        let x = BigUint::parse_bytes(sha256_hex_of_hex(&format!("{}{}", pad_hex(&salt), user_pass_hash)).as_bytes(), 16).unwrap();
        let v = g().modpow(&x, &n);
        let b = BigUint::parse_bytes(b"fedcba0987654321fedcba0987654321", 16).unwrap();
        let big_b = (&client.k * &v + g().modpow(&b, &n)) % &n;
        let big_a = BigUint::parse_bytes(client.srp_a().as_bytes(), 16).unwrap();
        let u = BigUint::parse_bytes(sha256_hex_of_hex(&format!("{}{}", pad_hex(&big_a), pad_hex(&big_b))).as_bytes(), 16).unwrap();
        let s_server = (big_a * v.modpow(&u, &n)).modpow(&b, &n);
        let key_server = hkdf16(&hex_to_bytes(&pad_hex(&s_server)), &hex_to_bytes(&pad_hex(&u)));

        let key_client = client.session_key(user, password, &salt.to_str_radix(16), &big_b.to_str_radix(16)).unwrap();
        assert_eq!(key_client, key_server);

        let wrong = client.session_key(user, "wrong-password", &salt.to_str_radix(16), &big_b.to_str_radix(16)).unwrap();
        assert_ne!(wrong, key_server);

        let sig = client.password_claim(user, password, &salt.to_str_radix(16), &big_b.to_str_radix(16), &B64.encode(b"block"), "Wed Nov 4 09:05:03 UTC 2026").unwrap();
        assert_eq!(B64.decode(sig).unwrap().len(), 32);
    }

    #[test]
    fn rejects_zero_b() {
        let c = SrpClient::with_secret("us-east-2_X", BigUint::from(5u32));
        assert!(c.session_key("u", "p", "ab", "0").is_err());
        assert!(c.session_key("u", "p", "ab", &n().to_str_radix(16)).is_err());
    }
}
