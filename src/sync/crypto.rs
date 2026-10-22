//! Secrets and encryption. A secret is 32 random bytes; the log id, the token the server
//! checks and the encryption key are all derived from it, so the server never sees the key

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub [u8; 32]);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret({}…)", &self.log_id()[..6])
    }
}

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::getrandom(&mut bytes).expect("no random numbers");
    bytes
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Secret {
    pub fn new() -> Secret {
        Secret(random())
    }

    pub fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    pub fn decode(text: &str) -> Result<Secret> {
        let bytes = URL_SAFE_NO_PAD.decode(text.trim()).map_err(|_| "Not a fooit secret".to_string())?;
        Ok(Secret(bytes.try_into().map_err(|_| "Not a fooit secret".to_string())?))
    }

    fn derive(&self, what: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        Hkdf::<Sha256>::new(None, &self.0).expand(format!("fooit {what}").as_bytes(), &mut out).unwrap();
        out
    }

    /// The name of the log on the server
    pub fn log_id(&self) -> String {
        hex(&self.derive("log")[..16])
    }

    /// What the server checks before it hands out or takes data
    pub fn auth(&self) -> String {
        hex(&self.derive("auth"))
    }

    /// The server keeps only this, so it can check the token without being able to make one
    pub fn auth_hash(&self) -> String {
        hex(&Sha256::digest(self.auth().as_bytes()))
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(&self.derive("key").into())
    }

    /// Nonce and ciphertext, bound to this log and to what the data is for
    pub fn seal(&self, purpose: &str, plain: &[u8]) -> Vec<u8> {
        let nonce: [u8; 24] = random();
        let aad = format!("{} {}", self.log_id(), purpose);
        let mut out = nonce.to_vec();
        out.extend(
            self.cipher()
                .encrypt(&XNonce::try_from(&nonce[..]).unwrap(), Payload { msg: plain, aad: aad.as_bytes() })
                .expect("encryption failed"),
        );
        out
    }

    pub fn open(&self, purpose: &str, sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() < 24 {
            return Err("Damaged sync data".into());
        }
        let aad = format!("{} {}", self.log_id(), purpose);
        self.cipher()
            .decrypt(&XNonce::try_from(&sealed[..24]).unwrap(), Payload { msg: &sealed[24..], aad: aad.as_bytes() })
            .map_err(|_| "Sync data that this secret does not open".to_string())
    }
}

/// A link to share: the server's address, with the secret after a `#` so that it is never
/// sent to a server when the link is opened
pub fn link(endpoint: &str, secret: &Secret) -> String {
    format!("{}/#{}", endpoint.trim_end_matches('/'), secret.encode())
}

pub fn parse_link(link: &str) -> Result<(String, Secret)> {
    let (endpoint, secret) = link.trim().rsplit_once('#').ok_or("A link looks like https://…/#secret")?;
    let endpoint = endpoint.trim_end_matches('/');
    if !(endpoint.starts_with("https://") || endpoint.starts_with("http://")) {
        return Err("A link looks like https://…/#secret".into());
    }
    Ok((endpoint.to_string(), Secret::decode(secret)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_and_open() {
        let s = Secret::new();
        let sealed = s.seal("batch", b"hello");
        assert_eq!(s.open("batch", &sealed).unwrap(), b"hello");
        assert!(s.open("base", &sealed).is_err());
        assert!(Secret::new().open("batch", &sealed).is_err());
        assert_ne!(s.log_id(), Secret::new().log_id());
        assert_eq!(Secret::decode(&s.encode()).unwrap(), s);
    }

    #[test]
    fn links() {
        let s = Secret::new();
        let l = link("https://sync.example.com/", &s);
        assert_eq!(parse_link(&l).unwrap(), ("https://sync.example.com".to_string(), s));
        assert!(parse_link("nonsense").is_err());
    }
}
