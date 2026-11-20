//! Send to phone: the code, its one-time token and the seal on the sessions.
//!
//! The QR code (`Offer`) holds the desktop's addresses, port, a one-time
//! token and a fresh key -- never cookies. The phone presents the token; the
//! desktop redeems it once (`Ticket`) and answers with the sessions sealed
//! under the key, which travels only through the camera.
//!
//! A symmetric key rather than a key exchange on purpose: the phone cannot
//! authenticate itself over the wire, so anyone in the middle who read the
//! token could offer a key of their own.
//!
//! Pure; the sockets are in `transfer_net`.

use std::net::Ipv4Addr;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bridge::protocol::{Cookie, Site};

/// Long enough to open the scanner; short enough that a photo of the screen
/// soon is worthless.
pub const TTL_SECS: i64 = 120;

/// Slack for the phone's clock. The desktop's clock enforces `TTL_SECS`; this
/// only stops an old picture of a code being taken.
pub const CLOCK_SKEW_SECS: i64 = 120;

pub const TOKEN_BYTES: usize = 16;
pub const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;

/// A desktop on Wi-Fi and Ethernet has more than one plausible address; more
/// would make the code hard to scan.
pub const MAX_ADDRESSES: usize = 4;

pub const PATH_PREFIX: &str = "/umd-transfer/v1/";
const SCHEME: &str = "umdplus";
const HOST: &str = "transfer";
const VERSION: u32 = 1;

/// Bound into every seal, so a ciphertext made for anything else, or another
/// token, does not open.
const CONTEXT: &[u8] = b"umdplus-session-transfer-v1";

/// Five jars are tens of kilobytes; anything near this is not ours.
pub const MAX_SEALED_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub struct Offer {
    pub addresses: Vec<Ipv4Addr>,
    pub port: u16,
    pub token: [u8; TOKEN_BYTES],
    pub key: [u8; KEY_BYTES],
    /// Seconds since the epoch, by the desktop's clock.
    pub expires_at: i64,
}

// Hand-written so neither secret can reach a log through `{:?}`.
impl std::fmt::Debug for Offer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Offer")
            .field("addresses", &self.addresses)
            .field("port", &self.port)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// Why a code or an answer was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Refusal {
    /// Not our code, or one naming a host off the local network.
    Invalid,
    Expired,
    /// Already taken, by this phone or by the desktop's record.
    Used,
    /// No address answered: another network, or a firewall.
    Unreachable,
    Rejected,
    /// Does not open with the code's key, or contradicts the code.
    Tampered,
    Empty,
}

pub fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the operating system's random source failed");
    bytes
}

/// Private, carrier-grade (tailnets, hotspots) or link-local IPv4. A code is
/// untrusted text; one naming a public host would send the token there.
pub fn local_address(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_private() || ip.is_link_local() || (a == 100 && (64..128).contains(&b))
}

impl Offer {
    pub fn new(addresses: Vec<Ipv4Addr>, port: u16, now: i64) -> Self {
        Self {
            addresses,
            port,
            token: random(),
            key: random(),
            expires_at: now + TTL_SECS,
        }
    }

    pub fn token_text(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.token)
    }

    /// A URI, so another camera app shows a link it cannot open instead of
    /// offering a web search for it.
    pub fn to_uri(&self) -> String {
        let addresses: Vec<String> = self.addresses.iter().map(Ipv4Addr::to_string).collect();
        format!(
            "{SCHEME}://{HOST}?v={VERSION}&a={}&p={}&t={}&k={}&e={}",
            addresses.join(","),
            self.port,
            self.token_text(),
            URL_SAFE_NO_PAD.encode(self.key),
            self.expires_at,
        )
    }

    pub fn parse(text: &str) -> Result<Self, Refusal> {
        let url = reqwest::Url::parse(text.trim()).map_err(|_| Refusal::Invalid)?;
        if url.scheme() != SCHEME || url.host_str() != Some(HOST) {
            return Err(Refusal::Invalid);
        }
        let field = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
                .ok_or(Refusal::Invalid)
        };
        if field("v")?.parse::<u32>().ok() != Some(VERSION) {
            return Err(Refusal::Invalid);
        }
        let addresses = field("a")?
            .split(',')
            .map(|part| part.parse::<Ipv4Addr>().map_err(|_| Refusal::Invalid))
            .collect::<Result<Vec<_>, _>>()?;
        if addresses.is_empty()
            || addresses.len() > MAX_ADDRESSES
            || !addresses.iter().all(|ip| local_address(*ip))
        {
            return Err(Refusal::Invalid);
        }
        let port = field("p")?.parse::<u16>().map_err(|_| Refusal::Invalid)?;
        if port == 0 {
            return Err(Refusal::Invalid);
        }
        let token = decode_fixed::<TOKEN_BYTES>(&field("t")?)?;
        let key = decode_fixed::<KEY_BYTES>(&field("k")?)?;
        let expires_at = field("e")?.parse::<i64>().map_err(|_| Refusal::Invalid)?;
        Ok(Self {
            addresses,
            port,
            token,
            key,
            expires_at,
        })
    }

    pub fn check_fresh(&self, now: i64) -> Result<(), Refusal> {
        if now > self.expires_at + CLOCK_SKEW_SECS {
            Err(Refusal::Expired)
        } else {
            Ok(())
        }
    }
}

fn decode_fixed<const N: usize>(text: &str) -> Result<[u8; N], Refusal> {
    URL_SAFE_NO_PAD
        .decode(text)
        .ok()
        .and_then(|bytes| <[u8; N]>::try_from(bytes.as_slice()).ok())
        .ok_or(Refusal::Invalid)
}

/// Constant time, so timing reveals nothing about a guessed token.
fn same_bytes(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn aad(token: &[u8; TOKEN_BYTES]) -> Vec<u8> {
    [CONTEXT, token.as_slice()].concat()
}

/// `nonce || ciphertext`, sealed under `key` and bound to `token`.
pub fn seal(key: &[u8; KEY_BYTES], token: &[u8; TOKEN_BYTES], plain: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    // Random even though each key seals once: a retry must never reuse a nonce.
    let nonce: [u8; NONCE_BYTES] = random();
    let sealed = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plain,
                aad: &aad(token),
            },
        )
        .expect("ChaCha20-Poly1305 encryption cannot fail for an in-memory buffer");
    [nonce.as_slice(), &sealed].concat()
}

pub fn open(key: &[u8; KEY_BYTES], token: &[u8; TOKEN_BYTES], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < NONCE_BYTES || sealed.len() > MAX_SEALED_BYTES {
        return None;
    }
    let (nonce, body) = sealed.split_at(NONCE_BYTES);
    ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: body,
                aad: &aad(token),
            },
        )
        .ok()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentSession {
    pub site: Site,
    pub cookies: Vec<Cookie>,
    pub captured_at: i64,
    #[serde(default)]
    pub account_hint: Option<String>,
    /// The site an other-site jar is for.
    #[serde(default)]
    pub domain: Option<String>,
}

/// Token and expiry repeated inside the seal, where the key vouches for them;
/// the code's own copy is only as trustworthy as whoever printed it.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bundle {
    pub v: u32,
    pub token: String,
    pub expires_at: i64,
    pub sessions: Vec<SentSession>,
}

impl Bundle {
    pub fn for_offer(offer: &Offer, sessions: Vec<SentSession>) -> Self {
        Self {
            v: VERSION,
            token: offer.token_text(),
            expires_at: offer.expires_at,
            sessions,
        }
    }
}

pub fn open_bundle(offer: &Offer, sealed: &[u8]) -> Result<Bundle, Refusal> {
    let mut plain = open(&offer.key, &offer.token, sealed).ok_or(Refusal::Tampered)?;
    let bundle = serde_json::from_slice::<Bundle>(&plain);
    plain.fill(0);
    let bundle = bundle.map_err(|_| Refusal::Tampered)?;
    if bundle.v != VERSION
        || !same_bytes(bundle.token.as_bytes(), offer.token_text().as_bytes())
        || bundle.expires_at != offer.expires_at
    {
        return Err(Refusal::Tampered);
    }
    Ok(bundle)
}

/// The desktop's side of a code: one use, before expiry.
pub struct Ticket {
    token: [u8; TOKEN_BYTES],
    expires_at: i64,
    used: bool,
}

impl Ticket {
    pub fn new(offer: &Offer) -> Self {
        Self {
            token: offer.token,
            expires_at: offer.expires_at,
            used: false,
        }
    }

    /// Spent before anything is sent: a repeat of the token, whether a retry
    /// or someone who saw the first request, gets nothing.
    pub fn redeem(&mut self, presented: &str, now: i64) -> Result<(), Refusal> {
        let matches = decode_fixed::<TOKEN_BYTES>(presented)
            .is_ok_and(|bytes| same_bytes(&bytes, &self.token));
        if !matches {
            return Err(Refusal::Invalid);
        }
        if self.used {
            return Err(Refusal::Used);
        }
        if now > self.expires_at {
            return Err(Refusal::Expired);
        }
        self.used = true;
        Ok(())
    }

    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }
}

pub fn token_in_path(path: &str) -> Option<&str> {
    let token = path.strip_prefix(PATH_PREFIX)?;
    (!token.is_empty() && token.len() <= 64 && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        .then_some(token)
}

/// Digests of the tokens this phone has taken, kept until `check_fresh` would
/// refuse them anyway. The desktop forgets a used token when it closes.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SpentTokens {
    #[serde(default)]
    entries: Vec<SpentToken>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpentToken {
    digest: String,
    forget_after: i64,
}

const SPENT_KEPT: usize = 64;

impl SpentTokens {
    fn digest(token: &[u8; TOKEN_BYTES]) -> String {
        hex::encode(Sha256::digest(token))
    }

    pub fn contains(&self, token: &[u8; TOKEN_BYTES]) -> bool {
        let digest = Self::digest(token);
        self.entries.iter().any(|entry| entry.digest == digest)
    }

    pub fn note(&mut self, offer: &Offer) {
        if self.contains(&offer.token) {
            return;
        }
        if self.entries.len() >= SPENT_KEPT {
            self.entries.remove(0);
        }
        self.entries.push(SpentToken {
            digest: Self::digest(&offer.token),
            forget_after: offer.expires_at + CLOCK_SKEW_SECS,
        });
    }

    pub fn prune(&mut self, now: i64) {
        self.entries.retain(|entry| entry.forget_after >= now);
    }
}

pub fn check_before_fetch(offer: &Offer, spent: &SpentTokens, now: i64) -> Result<(), Refusal> {
    offer.check_fresh(now)?;
    if spent.contains(&offer.token) {
        return Err(Refusal::Used);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_900_000_000;

    fn offer() -> Offer {
        Offer::new(vec![Ipv4Addr::new(192, 168, 1, 20), Ipv4Addr::new(10, 0, 0, 5)], 50123, NOW)
    }

    fn cookie() -> Cookie {
        Cookie {
            domain: ".youtube.com".into(),
            name: "SID".into(),
            value: "secret-value".into(),
            path: "/".into(),
            secure: true,
            http_only: true,
            expiration_date: Some(1_950_000_000.0),
        }
    }

    fn sealed_bundle(offer: &Offer) -> Vec<u8> {
        let bundle = Bundle::for_offer(
            offer,
            vec![SentSession {
                site: Site::Youtube,
                cookies: vec![cookie()],
                captured_at: NOW - 60,
                account_hint: Some("m•••@gmail.com".into()),
                domain: None,
            }],
        );
        seal(&offer.key, &offer.token, &serde_json::to_vec(&bundle).unwrap())
    }

    #[test]
    fn a_code_reads_back_as_it_was_written() {
        let offer = offer();
        let uri = offer.to_uri();
        assert!(uri.starts_with("umdplus://transfer?v=1&a=192.168.1.20,10.0.0.5&p=50123&t="));
        assert_eq!(Offer::parse(&uri).unwrap(), offer);
        assert_eq!(offer.expires_at, NOW + TTL_SECS);
        assert!(uri.len() < 200, "{}", uri.len());
    }

    #[test]
    fn two_codes_never_share_a_token_or_a_key() {
        let (a, b) = (offer(), offer());
        assert_ne!(a.token, b.token);
        assert_ne!(a.key, b.key);
    }

    #[test]
    fn the_secrets_stay_out_of_debug_output() {
        let offer = offer();
        let shown = format!("{offer:?}");
        assert!(!shown.contains(&offer.token_text()));
        assert!(!shown.contains(&URL_SAFE_NO_PAD.encode(offer.key)));
    }

    #[test]
    fn a_code_that_is_not_whole_or_not_ours_is_refused() {
        let good = offer().to_uri();
        let cases = [
            good.replace("umdplus://", "https://"),
            good.replace("//transfer", "//elsewhere"),
            good.replace("v=1", "v=2"),
            good.replace("192.168.1.20", "8.8.8.8"),
            good.replace("192.168.1.20,10.0.0.5", "127.0.0.1"),
            good.replace("192.168.1.20,10.0.0.5", ""),
            good.replace("192.168.1.20,10.0.0.5", "10.0.0.1,10.0.0.2,10.0.0.3,10.0.0.4,10.0.0.5"),
            good.replace("p=50123", "p=0"),
            good.replace("p=50123", "p=70000"),
            good.replace("&t=", "&t=AA"),
            good.replace("&k=", "&k=A"),
            good.replace("&e=", "&e=x"),
            "not a code".to_string(),
            String::new(),
        ];
        for case in cases {
            assert_eq!(Offer::parse(&case), Err(Refusal::Invalid), "{case}");
        }
    }

    #[test]
    fn only_addresses_on_a_local_network_are_taken() {
        for ip in ["10.1.2.3", "172.16.0.1", "172.31.255.1", "192.168.0.10", "169.254.3.4", "100.64.0.1", "100.127.1.1"] {
            assert!(local_address(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "172.32.0.1", "100.128.0.1", "127.0.0.1", "0.0.0.0", "255.255.255.255", "224.0.0.1"] {
            assert!(!local_address(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn a_sealed_answer_opens_only_with_its_key_and_token() {
        let offer = offer();
        let sealed = seal(&offer.key, &offer.token, b"hello");
        assert_eq!(open(&offer.key, &offer.token, &sealed).as_deref(), Some(&b"hello"[..]));

        let other = Offer::new(offer.addresses.clone(), offer.port, NOW);
        assert!(open(&other.key, &offer.token, &sealed).is_none());
        assert!(open(&offer.key, &other.token, &sealed).is_none());

        let mut flipped = sealed.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 1;
        assert!(open(&offer.key, &offer.token, &flipped).is_none());
        assert!(open(&offer.key, &offer.token, &sealed[..8]).is_none());
    }

    #[test]
    fn sealing_the_same_thing_twice_looks_different() {
        let offer = offer();
        assert_ne!(seal(&offer.key, &offer.token, b"x"), seal(&offer.key, &offer.token, b"x"));
    }

    #[test]
    fn the_cookies_are_not_readable_in_what_travels() {
        let offer = offer();
        let sealed = sealed_bundle(&offer);
        let text = String::from_utf8_lossy(&sealed);
        assert!(!text.contains("secret-value"));
        assert!(!text.contains("SID"));
    }

    #[test]
    fn the_phone_takes_an_answer_made_for_its_code() {
        let offer = offer();
        let bundle = open_bundle(&offer, &sealed_bundle(&offer)).unwrap();
        assert_eq!(bundle.sessions.len(), 1);
        assert_eq!(bundle.sessions[0].site, Site::Youtube);
        assert_eq!(bundle.sessions[0].cookies[0].value, "secret-value");
    }

    #[test]
    fn the_phone_refuses_an_answer_that_names_another_code() {
        let offer = offer();
        // Right key, wrong token or expiry: an old answer served again.
        let mut stale = Bundle::for_offer(&offer, Vec::new());
        stale.expires_at -= 1;
        let sealed = seal(&offer.key, &offer.token, &serde_json::to_vec(&stale).unwrap());
        assert_eq!(open_bundle(&offer, &sealed).err(), Some(Refusal::Tampered));

        let mut other_token = Bundle::for_offer(&offer, Vec::new());
        other_token.token = Offer::new(vec![], 1, NOW).token_text();
        let sealed = seal(&offer.key, &offer.token, &serde_json::to_vec(&other_token).unwrap());
        assert_eq!(open_bundle(&offer, &sealed).err(), Some(Refusal::Tampered));

        let sealed = seal(&offer.key, &offer.token, b"{not json");
        assert_eq!(open_bundle(&offer, &sealed).err(), Some(Refusal::Tampered));
    }

    #[test]
    fn a_ticket_is_taken_once_and_only_in_time() {
        let offer = offer();
        let token = offer.token_text();

        let mut ticket = Ticket::new(&offer);
        assert_eq!(ticket.redeem("AAAAAAAAAAAAAAAAAAAAAA", NOW), Err(Refusal::Invalid));
        assert_eq!(ticket.redeem("garbage", NOW), Err(Refusal::Invalid));
        // A wrong guess does not spend it.
        assert_eq!(ticket.redeem(&token, NOW), Ok(()));
        assert_eq!(ticket.redeem(&token, NOW), Err(Refusal::Used));

        let mut late = Ticket::new(&offer);
        assert_eq!(late.redeem(&token, NOW + TTL_SECS + 1), Err(Refusal::Expired));
        let mut last_second = Ticket::new(&offer);
        assert_eq!(last_second.redeem(&token, NOW + TTL_SECS), Ok(()));
    }

    #[test]
    fn only_our_path_carries_a_token() {
        assert_eq!(token_in_path("/umd-transfer/v1/abc_-9"), Some("abc_-9"));
        for path in ["/", "/umd-transfer/v1/", "/umd-transfer/v2/abc", "/umd-transfer/v1/a/b", "/umd-transfer/v1/a%20b"] {
            assert_eq!(token_in_path(path), None, "{path}");
        }
        assert_eq!(token_in_path(&format!("{PATH_PREFIX}{}", "a".repeat(65))), None);
    }

    #[test]
    fn the_phone_refuses_an_expired_code() {
        let offer = offer();
        assert_eq!(offer.check_fresh(NOW), Ok(()));
        assert_eq!(offer.check_fresh(offer.expires_at + CLOCK_SKEW_SECS), Ok(()));
        assert_eq!(offer.check_fresh(offer.expires_at + CLOCK_SKEW_SECS + 1), Err(Refusal::Expired));
        let spent = SpentTokens::default();
        assert_eq!(
            check_before_fetch(&offer, &spent, offer.expires_at + CLOCK_SKEW_SECS + 1),
            Err(Refusal::Expired)
        );
    }

    #[test]
    fn the_phone_refuses_a_code_it_already_took() {
        let offer = offer();
        let mut spent = SpentTokens::default();
        assert_eq!(check_before_fetch(&offer, &spent, NOW), Ok(()));
        spent.note(&offer);
        spent.note(&offer);
        assert_eq!(spent.entries.len(), 1);
        assert_eq!(check_before_fetch(&offer, &spent, NOW), Err(Refusal::Used));
        assert!(!spent.contains(&Offer::new(vec![], 1, NOW).token));

        spent.prune(offer.expires_at + CLOCK_SKEW_SECS);
        assert!(spent.contains(&offer.token));
        spent.prune(offer.expires_at + CLOCK_SKEW_SECS + 1);
        assert!(!spent.contains(&offer.token));
    }

    #[test]
    fn the_spent_list_keeps_digests_not_tokens() {
        let offer = offer();
        let mut spent = SpentTokens::default();
        spent.note(&offer);
        let written = serde_json::to_string(&spent).unwrap();
        assert!(!written.contains(&offer.token_text()));
        assert!(!written.contains(&hex::encode(offer.token)));
        let back: SpentTokens = serde_json::from_str(&written).unwrap();
        assert!(back.contains(&offer.token));
    }

    #[test]
    fn the_spent_list_makes_room_for_new_tokens() {
        let mut spent = SpentTokens::default();
        let first = offer();
        spent.note(&first);
        for _ in 0..SPENT_KEPT {
            spent.note(&offer());
        }
        assert_eq!(spent.entries.len(), SPENT_KEPT);
        assert!(!spent.contains(&first.token));
    }
}
