//! The sockets behind `transfer`: the desktop's short-lived listener, and the
//! phone's one request to it.
//!
//! Plain HTTP on purpose: the body is sealed under a key only the QR code
//! carries, and a certificate the phone has never seen would add nothing.
//! There is no relay, so a phone on another network gets no answer.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Notify};

use crate::bridge::protocol::Site;
use crate::bridge::store;
use crate::bridge::transfer::{
    self, local_address, open_bundle, token_in_path, Bundle, Offer, Refusal, SentSession,
    SpentTokens, Ticket, MAX_ADDRESSES, MAX_SEALED_BYTES, TTL_SECS,
};
use crate::error::AppResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Waiting,
    Sending,
    Sent,
    Expired,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum Started {
    Ready {
        /// SVG data URI for an `<img>`, so no markup is injected into the page.
        qr: String,
        addresses: Vec<String>,
        port: u16,
        expires_at: i64,
        sites: Vec<Site>,
    },
    NoNetwork,
    NothingToSend,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum Received {
    Received { sites: Vec<Site> },
    Refused { reason: Refusal },
}

type OnChange = Arc<dyn Fn(Phase) + Send + Sync>;

struct Running {
    cancel: Arc<Notify>,
    phase: Arc<Mutex<Phase>>,
}

/// One listener at a time; a new one makes the previous code worthless.
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

/// The phone needs one; the rest is someone else on the network.
const MAX_CONNECTIONS: usize = 4;
/// A 128-bit token is never guessed, so a stream of wrong ones is probing.
const MAX_BAD_REQUESTS: usize = 16;
const MAX_HEAD_BYTES: usize = 4096;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(15);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The phone tries them in order: the default route's address, then home
/// ranges before those virtual adapters (WSL, Hyper-V, Docker) and tailnets
/// usually take, link-local last.
pub fn order_addresses(found: &[Ipv4Addr], primary: Option<Ipv4Addr>) -> Vec<Ipv4Addr> {
    let rank = |ip: &Ipv4Addr| {
        let [a, b, ..] = ip.octets();
        match (a, b) {
            _ if Some(*ip) == primary => 0,
            (192, 168) => 1,
            (10, _) => 2,
            (172, _) => 3,
            (100, _) => 4,
            _ => 5,
        }
    };
    let mut ordered: Vec<Ipv4Addr> = found.iter().copied().filter(|ip| local_address(*ip)).collect();
    ordered.sort_by_key(rank);
    ordered.dedup();
    ordered.truncate(MAX_ADDRESSES);
    ordered
}

/// A UDP "connect" only consults the routing table; nothing is sent.
fn primary_address() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) => Some(ip),
        IpAddr::V6(_) => None,
    }
}

fn lan_addresses() -> Vec<Ipv4Addr> {
    let found: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|interface| match interface.ip() {
            IpAddr::V4(ip) if !interface.is_loopback() => Some(ip),
            _ => None,
        })
        .collect();
    order_addresses(&found, primary_address())
}

fn qr_data_uri(text: &str) -> AppResult<String> {
    use qrcode::render::svg;
    use qrcode::{EcLevel, QrCode};

    let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M)
        .map_err(|err| crate::error::AppError::Other(format!("could not draw the code: {err}")))?;
    // Black on white whatever the theme: inverted codes scan poorly.
    let image = code
        .render::<svg::Color>()
        .min_dimensions(240, 240)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .quiet_zone(true)
        .build();
    Ok(format!("data:image/svg+xml;base64,{}", STANDARD.encode(image)))
}

/// Read when the phone asks, not held in memory while the code is shown.
fn sessions_for(sites: &[Site]) -> Vec<SentSession> {
    let link = crate::bridge::state::load();
    sites
        .iter()
        .filter_map(|&site| match store::export(site) {
            Ok(Some(jar)) => Some(SentSession {
                site,
                cookies: jar.cookies,
                captured_at: jar.captured_at,
                account_hint: (site == Site::Youtube).then(|| link.account_hint.clone()).flatten(),
                domain: jar.domain,
            }),
            Ok(None) => None,
            Err(err) => {
                crate::log_warn!("bridge", "could not read the {site:?} session for the phone: {err}");
                None
            }
        })
        .collect()
}

pub async fn start(sites: Vec<Site>, on_change: OnChange) -> AppResult<Started> {
    cancel();

    let mut chosen: Vec<Site> = Vec::new();
    for site in sites {
        if !chosen.contains(&site) && store::export(site).ok().flatten().is_some() {
            chosen.push(site);
        }
    }
    if chosen.is_empty() {
        return Ok(Started::NothingToSend);
    }

    let addresses = lan_addresses();
    if addresses.is_empty() {
        return Ok(Started::NoNetwork);
    }

    // Every interface, so an address the code does not list still works;
    // `allowed_peer` filters per connection.
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    let port = listener.local_addr()?.port();
    let offer = Offer::new(addresses, port, now());
    let qr = qr_data_uri(&offer.to_uri())?;

    let cancel = Arc::new(Notify::new());
    let phase = Arc::new(Mutex::new(Phase::Waiting));
    *lock(&RUNNING) = Some(Running {
        cancel: cancel.clone(),
        phase: phase.clone(),
    });

    let started = Started::Ready {
        qr,
        addresses: offer.addresses.iter().map(Ipv4Addr::to_string).collect(),
        port,
        expires_at: offer.expires_at,
        sites: chosen.clone(),
    };
    tokio::spawn(serve(listener, offer, chosen, cancel, phase, on_change));
    Ok(started)
}

pub fn cancel() {
    if let Some(running) = lock(&RUNNING).take() {
        running.cancel.notify_one();
    }
}

pub fn phase() -> Option<Phase> {
    lock(&RUNNING).as_ref().map(|running| *lock(&running.phase))
}

fn allowed_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => local_address(ip) || ip.is_loopback(),
        IpAddr::V6(ip) => ip.to_ipv4_mapped().is_some_and(|v4| local_address(v4) || v4.is_loopback()),
    }
}

struct Shared {
    ticket: Mutex<Ticket>,
    offer: Offer,
    sites: Vec<Site>,
    active: AtomicUsize,
    bad: AtomicUsize,
    done: mpsc::Sender<Phase>,
    phase: Arc<Mutex<Phase>>,
    on_change: OnChange,
}

impl Shared {
    fn set(&self, next: Phase) {
        *lock(&self.phase) = next;
        (self.on_change)(next);
    }
}

async fn serve(
    listener: TcpListener,
    offer: Offer,
    sites: Vec<Site>,
    cancel: Arc<Notify>,
    phase: Arc<Mutex<Phase>>,
    on_change: OnChange,
) {
    let (done, mut finished) = mpsc::channel::<Phase>(1);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(TTL_SECS as u64);
    let shared = Arc::new(Shared {
        ticket: Mutex::new(Ticket::new(&offer)),
        offer,
        sites,
        active: AtomicUsize::new(0),
        bad: AtomicUsize::new(0),
        done,
        phase,
        on_change,
    });

    let last = loop {
        tokio::select! {
            _ = cancel.notified() => break Phase::Cancelled,
            _ = tokio::time::sleep_until(deadline) => break Phase::Expired,
            Some(outcome) = finished.recv() => break outcome,
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else { continue };
                if !allowed_peer(peer.ip()) || shared.active.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    continue;
                }
                shared.active.fetch_add(1, Ordering::SeqCst);
                let shared = shared.clone();
                tokio::spawn(async move {
                    handle(stream, peer, &shared).await;
                    shared.active.fetch_sub(1, Ordering::SeqCst);
                });
            }
        }
    };
    drop(listener);
    shared.set(last);
}

async fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::with_capacity(512);
    let mut chunk = [0u8; 512];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        if head.len() > MAX_HEAD_BYTES {
            return None;
        }
        let read = tokio::time::timeout(READ_TIMEOUT, stream.read(&mut chunk)).await.ok()?.ok()?;
        if read == 0 {
            return None;
        }
        head.extend_from_slice(&chunk[..read]);
    }
    String::from_utf8(head).ok()
}

async fn reply(stream: &mut TcpStream, status: &str, body: &[u8]) -> bool {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let write = async {
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(body).await?;
        stream.flush().await?;
        stream.shutdown().await
    };
    matches!(tokio::time::timeout(WRITE_TIMEOUT, write).await, Ok(Ok(())))
}

async fn handle(mut stream: TcpStream, peer: SocketAddr, shared: &Shared) {
    let Some(head) = read_head(&mut stream).await else {
        return;
    };
    let mut parts = head.lines().next().unwrap_or("").split(' ');
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let Some(token) = token_in_path(path).filter(|_| method == "GET") else {
        reply(&mut stream, "404 Not Found", b"").await;
        return;
    };

    let redeemed = lock(&shared.ticket).redeem(token, now());
    match redeemed {
        Ok(()) => {}
        Err(Refusal::Used) => {
            reply(&mut stream, "409 Conflict", b"").await;
            return;
        }
        Err(Refusal::Expired) => {
            reply(&mut stream, "410 Gone", b"").await;
            return;
        }
        Err(_) => {
            reply(&mut stream, "404 Not Found", b"").await;
            if shared.bad.fetch_add(1, Ordering::SeqCst) + 1 >= MAX_BAD_REQUESTS {
                crate::log_warn!("bridge", "phone transfer closed after repeated wrong tokens");
                let _ = shared.done.try_send(Phase::Failed);
            }
            return;
        }
    }

    shared.set(Phase::Sending);
    let sites = shared.sites.clone();
    let sessions = tokio::task::spawn_blocking(move || sessions_for(&sites))
        .await
        .unwrap_or_default();
    let count = sessions.len();
    let mut plain = serde_json::to_vec(&Bundle::for_offer(&shared.offer, sessions)).unwrap_or_default();
    let sealed = transfer::seal(&shared.offer.key, &shared.offer.token, &plain);
    plain.fill(0);

    let sent = reply(&mut stream, "200 OK", &sealed).await;
    if sent {
        crate::log_info!("bridge", "sent {count} sessions to a phone at {}", peer.ip());
    } else {
        crate::log_warn!("bridge", "the phone at {} did not take the sessions", peer.ip());
    }
    let _ = shared.done.try_send(if sent { Phase::Sent } else { Phase::Failed });
}

// -- the phone's side ---------------------------------------------------------

fn spent_path() -> AppResult<std::path::PathBuf> {
    Ok(crate::bridge::dir()?.join("transfers-spent.json"))
}

fn load_spent() -> SpentTokens {
    spent_path()
        .ok()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_spent(spent: &SpentTokens) -> AppResult<()> {
    crate::bridge::state::write_atomic(&spent_path()?, &serde_json::to_vec(spent)?)
}

async fn fetch(offer: &Offer) -> Result<Vec<u8>, Refusal> {
    // An internet proxy could not reach the desktop and must not see the token.
    let client = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Refusal::Unreachable)?;

    for address in &offer.addresses {
        let url = format!(
            "http://{address}:{}{}{}",
            offer.port,
            transfer::PATH_PREFIX,
            offer.token_text()
        );
        let Ok(mut response) = client.get(&url).send().await else {
            continue;
        };
        return match response.status().as_u16() {
            200 => {
                let mut body = Vec::new();
                while let Ok(Some(chunk)) = response.chunk().await {
                    if body.len() + chunk.len() > MAX_SEALED_BYTES {
                        return Err(Refusal::Tampered);
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(body)
            }
            409 => Err(Refusal::Used),
            410 => Err(Refusal::Expired),
            _ => Err(Refusal::Rejected),
        };
    }
    Err(Refusal::Unreachable)
}

pub async fn receive(code: &str) -> Received {
    match receive_inner(code).await {
        Ok(sites) => Received::Received { sites },
        Err(reason) => {
            crate::log_info!("bridge", "a session transfer from the computer was refused: {reason:?}");
            Received::Refused { reason }
        }
    }
}

async fn receive_inner(code: &str) -> Result<Vec<Site>, Refusal> {
    let offer = Offer::parse(code)?;
    let mut spent = load_spent();
    spent.prune(now());
    transfer::check_before_fetch(&offer, &spent, now())?;

    let sealed = fetch(&offer).await?;
    let bundle = open_bundle(&offer, &sealed)?;

    // Before storing, so a failure part way still leaves the code spent.
    spent.note(&offer);
    if let Err(err) = save_spent(&spent) {
        crate::log_warn!("bridge", "could not record a spent transfer code: {err}");
    }

    let sites = tokio::task::spawn_blocking(move || {
        let mut kept = Vec::new();
        for session in bundle.sessions {
            let site = session.site;
            match store::receive(site, session.cookies, session.captured_at, session.domain, session.account_hint) {
                Ok(true) if !kept.contains(&site) => kept.push(site),
                Ok(_) => {}
                Err(err) => crate::log_warn!("bridge", "could not store the {site:?} session: {err}"),
            }
        }
        kept
    })
    .await
    .unwrap_or_default();

    if sites.is_empty() {
        return Err(Refusal::Empty);
    }
    crate::log_info!("bridge", "received sessions from the computer for {sites:?}");
    Ok(sites)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> Ipv4Addr {
        text.parse().unwrap()
    }

    #[test]
    fn the_route_out_comes_first_and_virtual_ranges_last() {
        let found = [ip("172.17.0.1"), ip("169.254.1.2"), ip("10.0.0.7"), ip("192.168.1.20"), ip("8.8.8.8"), ip("100.100.1.1")];
        assert_eq!(
            order_addresses(&found, None),
            vec![ip("192.168.1.20"), ip("10.0.0.7"), ip("172.17.0.1"), ip("100.100.1.1")]
        );
        assert_eq!(order_addresses(&found, Some(ip("10.0.0.7")))[0], ip("10.0.0.7"));
        assert!(order_addresses(&[ip("8.8.8.8")], Some(ip("8.8.8.8"))).is_empty());
    }

    #[test]
    fn only_peers_on_a_local_network_are_read() {
        assert!(allowed_peer("192.168.1.5".parse().unwrap()));
        assert!(allowed_peer("127.0.0.1".parse().unwrap()));
        assert!(allowed_peer("::ffff:10.0.0.2".parse().unwrap()));
        assert!(!allowed_peer("8.8.8.8".parse().unwrap()));
        assert!(!allowed_peer("2001:db8::1".parse().unwrap()));
    }

    #[tokio::test]
    async fn a_code_is_answered_once_and_sealed() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let offer = Offer::new(vec![ip("127.0.0.1")], port, now());
        let (done, mut finished) = mpsc::channel(1);
        let shared = Arc::new(Shared {
            ticket: Mutex::new(Ticket::new(&offer)),
            offer: offer.clone(),
            sites: Vec::new(),
            active: AtomicUsize::new(0),
            bad: AtomicUsize::new(0),
            done,
            phase: Arc::new(Mutex::new(Phase::Waiting)),
            on_change: Arc::new(|_| {}),
        });
        let server = {
            let shared = shared.clone();
            tokio::spawn(async move {
                for _ in 0..3 {
                    let (stream, peer) = listener.accept().await.unwrap();
                    handle(stream, peer, &shared).await;
                }
            })
        };

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let base = format!("http://127.0.0.1:{port}{}", transfer::PATH_PREFIX);

        let wrong = client.get(format!("{base}AAAAAAAAAAAAAAAAAAAAAA")).send().await.unwrap();
        assert_eq!(wrong.status().as_u16(), 404);

        let first = client.get(format!("{base}{}", offer.token_text())).send().await.unwrap();
        assert_eq!(first.status().as_u16(), 200);
        let sealed = first.bytes().await.unwrap();
        let bundle = open_bundle(&offer, &sealed).unwrap();
        assert!(bundle.sessions.is_empty());
        assert_eq!(finished.recv().await, Some(Phase::Sent));

        let again = client.get(format!("{base}{}", offer.token_text())).send().await.unwrap();
        assert_eq!(again.status().as_u16(), 409);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn the_phone_takes_nothing_from_a_code_it_cannot_reach() {
        // A code may not name loopback at all.
        let offer = Offer::new(vec![ip("127.0.0.1")], 9, now());
        assert_eq!(Offer::parse(&offer.to_uri()), Err(Refusal::Invalid));

        let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let offer = Offer::new(vec![ip("127.0.0.1")], port, now());
        assert_eq!(fetch(&offer).await, Err(Refusal::Unreachable));
    }
}
