//! Web Push: the VAPID identity the engine signs with (RFC 8292) and the
//! payload encryption every push service requires (RFC 8291, aes128gcm).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::{aead, agreement, hkdf, rand as ring_rand, signature};

/// The record size the encrypted body declares (one record holds it all).
const RECORD_SIZE: u32 = 4096;

#[derive(Debug)]
pub struct WebPushError(pub String);

impl std::fmt::Display for WebPushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn fail(what: &str) -> impl Fn(ring::error::Unspecified) -> WebPushError + '_ {
    move |_| WebPushError(what.to_string())
}

/// The engine's push identity: an ECDSA P-256 key, kept as PKCS#8.
pub struct VapidKey {
    pair: signature::EcdsaKeyPair,
}

impl VapidKey {
    pub fn generate_pkcs8() -> Result<Vec<u8>, WebPushError> {
        let rng = ring_rand::SystemRandom::new();
        signature::EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map(|document| document.as_ref().to_vec())
            .map_err(fail("generate VAPID key"))
    }

    pub fn from_pkcs8(pkcs8: &[u8]) -> Result<Self, WebPushError> {
        let rng = ring_rand::SystemRandom::new();
        signature::EcdsaKeyPair::from_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            pkcs8,
            &rng,
        )
        .map(|pair| Self { pair })
        .map_err(|err| WebPushError(format!("read VAPID key: {err}")))
    }

    /// The uncompressed public key, base64url: the browser's
    /// `applicationServerKey`.
    pub fn public_key(&self) -> String {
        use signature::KeyPair as _;
        URL_SAFE_NO_PAD.encode(self.pair.public_key().as_ref())
    }

    /// The `Authorization` header for a push to `endpoint`: a JWT for the
    /// endpoint's origin, valid for 12 hours, naming `subject` (a `mailto:`
    /// or `https:` contact).
    pub fn authorization(
        &self,
        endpoint: &str,
        subject: &str,
        now_s: u64,
    ) -> Result<String, WebPushError> {
        let audience =
            origin(endpoint).ok_or_else(|| WebPushError(format!("no origin in {endpoint}")))?;
        let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = URL_SAFE_NO_PAD.encode(
            serde_json::json!({ "aud": audience, "exp": now_s + 12 * 3600, "sub": subject })
                .to_string(),
        );
        let signing_input = format!("{header}.{claims}");
        let rng = ring_rand::SystemRandom::new();
        let signed = self
            .pair
            .sign(&rng, signing_input.as_bytes())
            .map_err(fail("sign VAPID token"))?;
        Ok(format!(
            "vapid t={signing_input}.{}, k={}",
            URL_SAFE_NO_PAD.encode(signed.as_ref()),
            self.public_key()
        ))
    }
}

/// `scheme://host[:port]` of a URL.
pub fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    (!authority.is_empty()).then(|| format!("{scheme}://{authority}"))
}

struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn expand(prk: &hkdf::Prk, info: &[&[u8]], out: &mut [u8]) -> Result<(), WebPushError> {
    prk.expand(info, Len(out.len()))
        .and_then(|okm| okm.fill(out))
        .map_err(fail("derive key"))
}

/// Encrypts `plaintext` for a browser subscription (`p256dh`: its 65-byte
/// public key; `auth`: its 16-byte secret) with a fresh sender key and salt.
pub fn encrypt(p256dh: &[u8], auth: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, WebPushError> {
    let rng = ring_rand::SystemRandom::new();
    let sender = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng)
        .map_err(fail("generate sender key"))?;
    let sender_public = sender
        .compute_public_key()
        .map_err(fail("sender public key"))?;
    let mut salt = [0u8; 16];
    ring_rand::SecureRandom::fill(&rng, &mut salt).map_err(fail("salt"))?;
    let ecdh_secret = agreement::agree_ephemeral(
        sender,
        &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, p256dh),
        |secret| secret.to_vec(),
    )
    .map_err(fail("key agreement with the subscription"))?;
    encrypt_with(
        &ecdh_secret,
        p256dh,
        sender_public.as_ref(),
        auth,
        &salt,
        plaintext,
    )
}

/// RFC 8291 §3.4 and RFC 8188 from an agreed secret: one record, no padding.
pub fn encrypt_with(
    ecdh_secret: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    auth_secret: &[u8],
    salt: &[u8; 16],
    plaintext: &[u8],
) -> Result<Vec<u8>, WebPushError> {
    if ua_public.len() != 65 || as_public.len() != 65 || auth_secret.len() != 16 {
        return Err(WebPushError(
            "a subscription needs a 65-byte p256dh and a 16-byte auth".into(),
        ));
    }
    if plaintext.len() + 1 + 16 > RECORD_SIZE as usize {
        return Err(WebPushError("payload too large for one record".into()));
    }
    let prk_key = hkdf::Salt::new(hkdf::HKDF_SHA256, auth_secret).extract(ecdh_secret);
    let mut ikm = [0u8; 32];
    expand(
        &prk_key,
        &[b"WebPush: info\0", ua_public, as_public],
        &mut ikm,
    )?;
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&ikm);
    let mut cek = [0u8; 16];
    expand(&prk, &[b"Content-Encoding: aes128gcm\0"], &mut cek)?;
    let mut nonce = [0u8; 12];
    expand(&prk, &[b"Content-Encoding: nonce\0"], &mut nonce)?;

    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_128_GCM, &cek).map_err(fail("content key"))?,
    );
    // The last (only) record ends with the 0x02 delimiter.
    let mut record = Vec::with_capacity(plaintext.len() + 17);
    record.extend_from_slice(plaintext);
    record.push(2);
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::empty(),
        &mut record,
    )
    .map_err(fail("encrypt"))?;

    let mut body = Vec::with_capacity(16 + 4 + 1 + 65 + record.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(65);
    body.extend_from_slice(as_public);
    body.extend_from_slice(&record);
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(text: &str) -> Vec<u8> {
        URL_SAFE_NO_PAD.decode(text).unwrap()
    }

    /// RFC 8291 Appendix A. The shared secret is the example's, checked
    /// independently with another ECDH implementation.
    #[test]
    fn encrypts_the_rfc_8291_example() {
        let body = encrypt_with(
            &b64("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs"),
            &b64("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4"),
            &b64("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8"),
            &b64("BTBZMqHH6r4Tts7J_aSIgg"),
            &b64("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap(),
            b"When I grow up, I want to be a watermelon",
        )
        .unwrap();
        assert_eq!(
            URL_SAFE_NO_PAD.encode(&body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    #[test]
    fn a_fresh_encryption_has_the_rfc_8188_header() {
        let rng = ring_rand::SystemRandom::new();
        let browser =
            agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
        let p256dh = browser.compute_public_key().unwrap();
        let body = encrypt(p256dh.as_ref(), &[7u8; 16], b"{}").unwrap();
        assert_eq!(&body[16..20], &RECORD_SIZE.to_be_bytes());
        assert_eq!(body[20], 65);
        assert_eq!(body.len(), 16 + 4 + 1 + 65 + 2 + 1 + 16);
        assert!(
            encrypt(&[4u8; 10], &[7u8; 16], b"{}").is_err(),
            "a malformed key is refused"
        );
    }

    #[test]
    fn the_vapid_token_names_the_push_origin_and_verifies() {
        let key = VapidKey::from_pkcs8(&VapidKey::generate_pkcs8().unwrap()).unwrap();
        let header = key
            .authorization(
                "https://fcm.googleapis.com/fcm/send/abc",
                "mailto:ops@example.com",
                1_000,
            )
            .unwrap();
        let (token, k) = header
            .strip_prefix("vapid t=")
            .unwrap()
            .split_once(", k=")
            .unwrap();
        assert_eq!(k, key.public_key());
        let mut parts = token.split('.');
        let (head, claims, sig) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        let claims: serde_json::Value = serde_json::from_slice(&b64(claims)).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["exp"], 1_000 + 12 * 3600);
        assert_eq!(claims["sub"], "mailto:ops@example.com");
        signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, b64(k))
            .verify(
                format!("{head}.{}", token.split('.').nth(1).unwrap()).as_bytes(),
                &b64(sig),
            )
            .expect("signature verifies with the advertised key");
    }
}
