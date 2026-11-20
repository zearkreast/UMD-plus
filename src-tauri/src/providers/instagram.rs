//! An Instagram account's videos, as a list to pick from. Read here because
//! the engine's profile reader is broken; the feed past the newest page needs
//! the session the extension lends.
//!
//! Every request is made with the user's own account, and Instagram restricts
//! accounts that read faster than a person would, so every limit below keeps a
//! wide margin. A list cut short says so; a restriction cannot be undone.

use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use serde_json::Value;

use crate::error::{AppError, AppResult};
use crate::model::{MediaKind, MediaMetadata, PlatformId, TrackSummary, WatermarkSupport};
use crate::settings::Settings;
use crate::{bridge, log_warn};

pub const PROVIDER_ID: &str = "instagram";

/// The web app's own id, which both addresses expect in `X-IG-App-ID`.
const WEB_APP_ID: &str = "936619743392459";

/// Posts per feed page: what instagram.com itself asks for.
const PAGE_SIZE: u32 = 12;

/// Pages per reading, the profile's own included: at most 180 posts. More is
/// left unlisted rather than asked for in one long burst.
const PAGES_PER_READING: usize = 15;

/// The wait before each feed page, plus up to `PAGE_JITTER_MS` more: uneven,
/// and slower than a person scrolling.
const PAGE_DELAY: Duration = Duration::from_secs(8);
const PAGE_JITTER_MS: u64 = 5_000;

/// Requests allowed in any hour and any day, across every reading and every
/// subscription check. Far under what a browser makes while scrolling.
const HOURLY_BUDGET: usize = 30;
const DAILY_BUDGET: usize = 120;

/// Downloads of posts, each of which the engine reads from Instagram first:
/// one at a time, `DOWNLOAD_GAP` plus up to `DOWNLOAD_JITTER_MS` apart, and no
/// more than these in an hour and a day. A list of 180 videos takes hours.
const DOWNLOAD_GAP: Duration = Duration::from_secs(20);
const DOWNLOAD_JITTER_MS: u64 = 10_000;
const DOWNLOAD_HOURLY: usize = 40;
const DOWNLOAD_DAILY: usize = 200;

/// How long nothing is asked of Instagram after a refusal, a rate limit, a
/// checkpoint or an answer that is not the JSON expected.
const PAUSE_AFTER_REFUSAL_SECS: i64 = 3 * 60 * 60;

/// Warnings the interface explains (see `MediaPreviewCard`).
pub const WARN_SIGN_IN: &str = "instagramSignIn";
pub const WARN_PARTIAL: &str = "instagramPartial";
pub const WARN_PAUSED: &str = "instagramPaused";

/// Paths of instagram.com that are not an account's.
const NOT_ACCOUNTS: &[&str] = &[
    "about", "accounts", "api", "challenge", "developer", "direct", "explore", "graphql", "legal",
    "p", "privacy", "reel", "reels", "s", "stories", "tv", "web",
];

/// How much of a profile to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// As many pages as a reading allows: a person picking from the list.
    Full,
    /// The profile's newest page alone, one request: a subscription check,
    /// which only looks for what is new.
    Newest,
}

/// Requests made in the last day, against an hourly and a daily allowance.
struct Budget {
    sent: VecDeque<i64>,
    paused_until: i64,
    hourly: usize,
    daily: usize,
}

impl Budget {
    const fn new(hourly: usize, daily: usize) -> Self {
        Self { sent: VecDeque::new(), paused_until: 0, hourly, daily }
    }
}

/// The profile reader's, whose pause also stops downloads from starting.
static BUDGET: StdMutex<Budget> = StdMutex::new(Budget::new(HOURLY_BUDGET, DAILY_BUDGET));
static DOWNLOADS: StdMutex<Budget> = StdMutex::new(Budget::new(DOWNLOAD_HOURLY, DOWNLOAD_DAILY));

/// Held by the download whose turn it is, with when the last one started.
static DOWNLOAD_TURN: tokio::sync::Mutex<Option<std::time::Instant>> = tokio::sync::Mutex::const_new(None);

/// One reading at a time, so two lists opened together do not double the rate.
static READING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl Budget {
    /// Whether a request may go at `now`; if so it is counted.
    fn take(&mut self, now: i64) -> bool {
        while self.sent.front().is_some_and(|at| now - at >= 24 * 60 * 60) {
            self.sent.pop_front();
        }
        let last_hour = self.sent.iter().filter(|at| now - **at < 60 * 60).count();
        if now < self.paused_until || last_hour >= self.hourly || self.sent.len() >= self.daily {
            return false;
        }
        self.sent.push_back(now);
        true
    }

    fn pause(&mut self, now: i64) {
        self.paused_until = self.paused_until.max(now + PAUSE_AFTER_REFUSAL_SECS);
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn take_request() -> bool {
    BUDGET.lock().map(|mut budget| budget.take(now())).unwrap_or(false)
}

fn pause_requests() {
    if let Ok(mut budget) = BUDGET.lock() {
        budget.pause(now());
    }
}

/// The account a link is the profile of: `instagram.com/name/`, or its
/// `reels/` tab. A post, a reel and every page that is not an account's are
/// not profiles.
pub fn profile_name(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    if !matches!(parsed.scheme(), "http" | "https") || !bridge::protocol::under_domain(host, "instagram.com") {
        return None;
    }
    let segments: Vec<&str> = parsed.path_segments()?.filter(|segment| !segment.is_empty()).collect();
    let (name, tab) = match segments.as_slice() {
        [name] => (*name, None),
        [name, tab] => (*name, Some(*tab)),
        _ => return None,
    };
    if tab.is_some_and(|tab| tab != "reels") {
        return None;
    }
    let valid = (1..=30).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
        && !NOT_ACCOUNTS.contains(&name.to_ascii_lowercase().as_str());
    valid.then(|| name.to_ascii_lowercase())
}

/// Read `url` as an account's videos, when it is a profile link; `None` for
/// any other link, which is left to the other readers.
pub async fn analyze(url: &str, settings: &Settings, depth: Depth) -> AppResult<Option<MediaMetadata>> {
    let Some(name) = profile_name(url) else {
        return Ok(None);
    };
    let _one_at_a_time = READING.lock().await;

    let profile_url = format!("https://www.instagram.com/{name}/");
    let cookie = bridge::other_cookie_header(settings, &profile_url);
    let client = crate::net::client(settings)?;
    let mut warnings = Vec::new();

    if !take_request() {
        return Err(paused_error());
    }
    let body = fetch(
        &client,
        &format!("https://www.instagram.com/api/v1/users/web_profile_info/?username={name}"),
        &profile_url,
        cookie.as_deref(),
    )
    .await?;
    let profile = parse_profile(&body)?;
    let mut rows = profile.rows.clone();

    let wants_more = depth == Depth::Full && profile.has_more;
    match (&cookie, wants_more) {
        (_, false) => {}
        (None, true) => warnings.push(WARN_SIGN_IN.to_string()),
        (Some(cookie), true) => {
            let mut cursor: Option<String> = None;
            let mut pages = 1;
            loop {
                if pages >= PAGES_PER_READING {
                    warnings.push(WARN_PARTIAL.to_string());
                    break;
                }
                tokio::time::sleep(PAGE_DELAY + jitter(PAGE_JITTER_MS)).await;
                if !take_request() {
                    warnings.push(WARN_PAUSED.to_string());
                    break;
                }
                let mut address = format!(
                    "https://www.instagram.com/api/v1/feed/user/{}/?count={PAGE_SIZE}",
                    profile.id
                );
                if let Some(cursor) = &cursor {
                    address.push_str(&format!("&max_id={}", urlencode(cursor)));
                }
                // A page that fails keeps what was listed before it. The pause
                // is already set by `fetch`.
                let page = match fetch(&client, &address, &profile_url, Some(cookie)).await {
                    Ok(body) => parse_feed(&body),
                    Err(err) => {
                        log_warn!("instagram", "stopped paging the profile: {err}");
                        warnings.push(WARN_PAUSED.to_string());
                        break;
                    }
                };
                let Ok(page) = page else {
                    pause_requests();
                    warnings.push(WARN_PAUSED.to_string());
                    break;
                };
                pages += 1;
                let known: std::collections::HashSet<String> = rows.iter().map(|row| row.code.clone()).collect();
                rows.extend(page.rows.into_iter().filter(|row| !known.contains(&row.code)));
                match page.next {
                    Some(next) if page.more => cursor = Some(next),
                    _ => break,
                }
            }
        }
    }

    let videos: Vec<Row> = rows.into_iter().filter(|row| row.video).collect();
    if videos.is_empty() {
        return Err(if profile.private && cookie.is_none() {
            AppError::Forbidden { status: 403, detail: "this account is private".into() }
        } else {
            AppError::Unsupported("no videos were found on this account".into())
        });
    }
    Ok(Some(listing(&name, &profile, videos, warnings, url)))
}

/// Wait for this download's turn to read an Instagram post, by the limits
/// above; any other address goes at once. `interrupted` is polled so a pause
/// or cancel ends the wait, as `AppError::Canceled`. While the profile reader
/// is paused nothing waits hours: the download fails and says why.
pub async fn pace_download(url: &str, interrupted: impl Fn() -> bool) -> AppResult<()> {
    let on_instagram = reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(|host| bridge::protocol::under_domain(host, "instagram.com")))
        .unwrap_or(false);
    if !on_instagram {
        return Ok(());
    }
    let mut last_start = DOWNLOAD_TURN.lock().await;
    let gap = DOWNLOAD_GAP + jitter(DOWNLOAD_JITTER_MS);
    loop {
        if interrupted() {
            return Err(AppError::Canceled);
        }
        if BUDGET.lock().map(|budget| now() < budget.paused_until).unwrap_or(false) {
            return Err(paused_error());
        }
        let spaced = last_start.is_none_or(|at| at.elapsed() >= gap);
        if spaced && DOWNLOADS.lock().map(|mut budget| budget.take(now())).unwrap_or(false) {
            *last_start = Some(std::time::Instant::now());
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// `limit_ms` at most, from the clock: no generator is needed for spacing
/// that only has to be uneven.
fn jitter(limit_ms: u64) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos() as u64)
        .unwrap_or(0);
    Duration::from_millis(nanos % limit_ms)
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn paused_error() -> AppError {
    AppError::Forbidden {
        status: 429,
        detail: "Instagram is being given a rest after too many requests; try again in a few hours".into(),
    }
}

/// One request, read as JSON text. Anything other than a plain answer pauses
/// every reading: Instagram says "slow down" with 429, with 401 and a login
/// page, and with a `checkpoint_required` or `feedback_required` body.
async fn fetch(client: &reqwest::Client, address: &str, referer: &str, cookie: Option<&str>) -> AppResult<String> {
    let mut request = client
        .get(address)
        .header("X-IG-App-ID", WEB_APP_ID)
        .header("X-Requested-With", "XMLHttpRequest")
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::REFERER, referer);
    if let Some(cookie) = cookie {
        request = request.header(reqwest::header::COOKIE, cookie);
    }
    let response = match request.send().await {
        Ok(response) => response,
        // The network, not Instagram: nothing to pause for.
        Err(err) => return Err(err.into()),
    };
    let status = response.status().as_u16();
    if status == 404 {
        return Err(AppError::NotFound { status, detail: "there is no Instagram account by this name".into() });
    }
    let body = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) || is_refusal(&body) {
        pause_requests();
        return Err(paused_error());
    }
    Ok(body)
}

/// A body that is Instagram turning the request away, whatever its status.
fn is_refusal(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return true;
    };
    let says = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default();
    says("status") == "fail"
        || ["checkpoint_required", "feedback_required", "login_required", "challenge_required"]
            .iter()
            .any(|word| says("message").contains(word) || value.get(*word).is_some())
        || value.get("require_login").and_then(Value::as_bool) == Some(true)
}

/// A post as a row of the list.
#[derive(Debug, Clone, PartialEq)]
struct Row {
    code: String,
    video: bool,
    reel: bool,
    caption: Option<String>,
    thumbnail: Option<String>,
    duration: Option<f64>,
    taken_at: Option<i64>,
}

impl Row {
    fn url(&self) -> String {
        let kind = if self.reel { "reel" } else { "p" };
        format!("https://www.instagram.com/{kind}/{}/", self.code)
    }
}

#[derive(Debug, Clone)]
struct Profile {
    id: String,
    full_name: Option<String>,
    picture: Option<String>,
    private: bool,
    has_more: bool,
    rows: Vec<Row>,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).filter(|text| !text.trim().is_empty()).map(str::to_string)
}

fn parse_profile(body: &str) -> AppResult<Profile> {
    let root: Value = serde_json::from_str(body).map_err(|err| AppError::Parse(format!("the profile was unreadable: {err}")))?;
    let user = root
        .pointer("/data/user")
        .filter(|user| user.is_object())
        .ok_or_else(|| AppError::NotFound { status: 404, detail: "there is no Instagram account by this name".into() })?;
    let id = text(user, "id").ok_or_else(|| AppError::Parse("the profile had no id".into()))?;
    let timeline = user.get("edge_owner_to_timeline_media");
    let rows = timeline
        .and_then(|timeline| timeline.get("edges"))
        .and_then(Value::as_array)
        .map(|edges| edges.iter().filter_map(|edge| edge.get("node")).filter_map(profile_row).collect())
        .unwrap_or_default();
    Ok(Profile {
        id,
        full_name: text(user, "full_name"),
        picture: text(user, "profile_pic_url_hd").or_else(|| text(user, "profile_pic_url")),
        private: user.get("is_private").and_then(Value::as_bool).unwrap_or(false),
        has_more: timeline
            .and_then(|timeline| timeline.pointer("/page_info/has_next_page"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        rows,
    })
}

fn profile_row(node: &Value) -> Option<Row> {
    Some(Row {
        code: text(node, "shortcode")?,
        video: node.get("is_video").and_then(Value::as_bool).unwrap_or(false),
        reel: text(node, "product_type").as_deref() == Some("clips"),
        caption: node
            .pointer("/edge_media_to_caption/edges/0/node/text")
            .and_then(Value::as_str)
            .map(str::to_string),
        thumbnail: text(node, "thumbnail_src").or_else(|| text(node, "display_url")),
        duration: node.get("video_duration").and_then(Value::as_f64),
        taken_at: node.get("taken_at_timestamp").and_then(Value::as_i64),
    })
}

struct FeedPage {
    rows: Vec<Row>,
    more: bool,
    next: Option<String>,
}

fn parse_feed(body: &str) -> AppResult<FeedPage> {
    let root: Value = serde_json::from_str(body).map_err(|err| AppError::Parse(format!("the feed was unreadable: {err}")))?;
    let items = root
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::Parse("the feed had no items".into()))?;
    Ok(FeedPage {
        rows: items.iter().filter_map(feed_row).collect(),
        more: root.get("more_available").and_then(Value::as_bool).unwrap_or(false),
        next: text(&root, "next_max_id"),
    })
}

/// `media_type` 2 is a video; 8 a carousel, a video when any of its items is.
fn feed_row(item: &Value) -> Option<Row> {
    let media_type = item.get("media_type").and_then(Value::as_u64);
    let video = match media_type {
        Some(2) => true,
        Some(8) => item
            .get("carousel_media")
            .and_then(Value::as_array)
            .is_some_and(|media| media.iter().any(|one| one.get("media_type").and_then(Value::as_u64) == Some(2))),
        _ => false,
    };
    Some(Row {
        code: text(item, "code")?,
        video,
        reel: text(item, "product_type").as_deref() == Some("clips"),
        caption: item.pointer("/caption/text").and_then(Value::as_str).map(str::to_string),
        thumbnail: item.pointer("/image_versions2/candidates/0/url").and_then(Value::as_str).map(str::to_string),
        duration: item.get("video_duration").and_then(Value::as_f64),
        taken_at: item.get("taken_at").and_then(Value::as_i64),
    })
}

/// A caption's first line, short enough to be a title; the post's code when
/// it has none.
fn title(row: &Row) -> String {
    row.caption
        .as_deref()
        .and_then(|caption| caption.lines().map(str::trim).find(|line| !line.is_empty()))
        .map(|line| {
            let short: String = line.chars().take(80).collect();
            if short.len() < line.len() { format!("{short}…") } else { short }
        })
        .unwrap_or_else(|| format!("Instagram {}", row.code))
}

fn blank(url: &str, canonical_url: &str) -> MediaMetadata {
    MediaMetadata {
        url: url.to_string(),
        canonical_url: canonical_url.to_string(),
        platform: PlatformId::Instagram,
        platform_label: PlatformId::Instagram.label().to_string(),
        provider_id: PROVIDER_ID.to_string(),
        media_kind: MediaKind::Video,
        title: String::new(),
        creator: None,
        description: None,
        thumbnail_url: None,
        duration_sec: None,
        view_count: None,
        like_count: None,
        upload_date: None,
        is_live: false,
        formats: Vec::new(),
        entry_count: None,
        watermark_support: WatermarkSupport::NotApplicable,
        range_fetchable: false,
        warnings: Vec::new(),
        entries: Vec::new(),
        tracks: Vec::new(),
        items: Vec::new(),
        music: None,
        listing: false,
        list_url: None,
        subtitles: Vec::new(),
        file_tags: Default::default(),
    }
}

/// The account's videos in the shape the engine gives a playlist, so the list,
/// its picking, its folder and subscribing to it all work as they do there.
fn listing(name: &str, profile: &Profile, videos: Vec<Row>, warnings: Vec<String>, requested_url: &str) -> MediaMetadata {
    let entries: Vec<MediaMetadata> = videos
        .iter()
        .map(|row| MediaMetadata {
            title: title(row),
            creator: Some(name.to_string()),
            thumbnail_url: row.thumbnail.clone(),
            duration_sec: row.duration,
            upload_date: row
                .taken_at
                .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
                .map(|at| at.format("%Y%m%d").to_string()),
            ..blank(&row.url(), &row.url())
        })
        .collect();
    let tracks = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| TrackSummary {
            position: index as u32 + 1,
            title: entry.title.clone(),
            artists: name.to_string(),
            duration_sec: entry.duration_sec,
            url: Some(entry.canonical_url.clone()),
        })
        .collect();
    let profile_url = format!("https://www.instagram.com/{name}/");
    MediaMetadata {
        media_kind: MediaKind::Gallery,
        title: profile.full_name.clone().map_or_else(|| name.to_string(), |full| format!("{full} (@{name})")),
        creator: Some(name.to_string()),
        thumbnail_url: profile.picture.clone().or_else(|| entries.iter().find_map(|entry| entry.thumbnail_url.clone())),
        entry_count: Some(entries.len() as u32),
        listing: true,
        warnings,
        tracks,
        entries,
        ..blank(requested_url, &profile_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_accounts_own_page_is_a_profile() {
        assert_eq!(profile_name("https://www.instagram.com/Gezgin.Ada/").as_deref(), Some("gezgin.ada"));
        assert_eq!(profile_name("https://instagram.com/gezgin_ada?igsh=x").as_deref(), Some("gezgin_ada"));
        assert_eq!(profile_name("https://www.instagram.com/gezgin.ada/reels/").as_deref(), Some("gezgin.ada"));
        for other in [
            "https://www.instagram.com/p/DQ3zR6-DPGm/",
            "https://www.instagram.com/reel/abc/",
            "https://www.instagram.com/reels/",
            "https://www.instagram.com/explore/",
            "https://www.instagram.com/stories/gezgin.ada/1/",
            "https://www.instagram.com/gezgin.ada/tagged/",
            "https://www.instagram.com/",
            "https://evilinstagram.com/gezgin.ada/",
            "https://www.instagram.com/bad-name/",
        ] {
            assert_eq!(profile_name(other), None, "{other}");
        }
    }

    #[test]
    fn the_budget_holds_to_its_hour_its_day_and_its_pause() {
        let mut budget = Budget::new(HOURLY_BUDGET, DAILY_BUDGET);
        let start = 1_000_000;
        for _ in 0..HOURLY_BUDGET {
            assert!(budget.take(start));
        }
        assert!(!budget.take(start + 60), "the hour is spent");
        assert!(budget.take(start + 60 * 60), "a new hour");

        let mut day = Budget::new(HOURLY_BUDGET, DAILY_BUDGET);
        let mut at = start;
        let mut taken = 0;
        while taken < DAILY_BUDGET + 10 && at < start + 24 * 60 * 60 {
            if day.take(at) {
                taken += 1;
            }
            at += 60;
        }
        assert_eq!(taken, DAILY_BUDGET, "never more than the day allows");
        assert!(day.take(start + 24 * 60 * 60 + 60), "a request a day old no longer counts");

        let mut paused = Budget::new(HOURLY_BUDGET, DAILY_BUDGET);
        paused.pause(start);
        assert!(!paused.take(start + PAUSE_AFTER_REFUSAL_SECS - 1));
        assert!(paused.take(start + PAUSE_AFTER_REFUSAL_SECS));
    }

    #[tokio::test]
    async fn only_instagram_addresses_wait_their_turn() {
        assert!(pace_download("https://www.youtube.com/watch?v=x", || true).await.is_ok());
        assert!(matches!(
            pace_download("https://www.instagram.com/reel/abc/", || true).await,
            Err(AppError::Canceled)
        ));
    }

    #[test]
    fn a_turned_away_answer_is_a_refusal_whatever_its_status() {
        assert!(is_refusal("<!DOCTYPE html><html>login</html>"));
        assert!(is_refusal(r#"{"message":"checkpoint_required","status":"fail"}"#));
        assert!(is_refusal(r#"{"message":"Please wait a few minutes before you try again.","status":"fail"}"#));
        assert!(is_refusal(r#"{"require_login":true}"#));
        assert!(!is_refusal(r#"{"data":{"user":{"id":"1"}},"status":"ok"}"#));
        assert!(!is_refusal(r#"{"items":[],"more_available":false,"status":"ok"}"#));
    }

    fn profile_body() -> String {
        serde_json::json!({
            "data": { "user": {
                "id": "4242",
                "full_name": "Gezgin Ada",
                "is_private": false,
                "profile_pic_url_hd": "https://cdn.test/pic.jpg",
                "edge_owner_to_timeline_media": {
                    "count": 40,
                    "page_info": { "has_next_page": true, "end_cursor": "c1" },
                    "edges": [
                        { "node": { "shortcode": "V1", "is_video": true, "product_type": "clips", "video_duration": 14.2,
                            "thumbnail_src": "https://cdn.test/v1.jpg", "taken_at_timestamp": 1_763_000_000,
                            "edge_media_to_caption": { "edges": [{ "node": { "text": "\nKapadokya'da bir sabah\n#gezi" } }] } } },
                        { "node": { "shortcode": "P1", "is_video": false, "display_url": "https://cdn.test/p1.jpg" } },
                        { "node": { "shortcode": "V2", "is_video": true, "product_type": "feed" } }
                    ]
                }
            } },
            "status": "ok"
        })
        .to_string()
    }

    #[test]
    fn a_profile_gives_its_id_and_newest_posts() {
        let profile = parse_profile(&profile_body()).unwrap();
        assert_eq!(profile.id, "4242");
        assert!(profile.has_more);
        assert_eq!(profile.rows.len(), 3);
        assert_eq!(profile.rows[0].url(), "https://www.instagram.com/reel/V1/");
        assert_eq!(profile.rows[2].url(), "https://www.instagram.com/p/V2/");
        assert!(matches!(
            parse_profile(r#"{"data":{"user":null},"status":"ok"}"#),
            Err(AppError::NotFound { .. })
        ));
    }

    #[test]
    fn a_feed_page_marks_videos_and_carousels_with_one() {
        let body = serde_json::json!({
            "items": [
                { "code": "A", "media_type": 2, "product_type": "clips", "video_duration": 9.5, "taken_at": 1_763_000_000,
                  "image_versions2": { "candidates": [{ "url": "https://cdn.test/a.jpg" }] } },
                { "code": "B", "media_type": 1 },
                { "code": "C", "media_type": 8, "carousel_media": [{ "media_type": 1 }, { "media_type": 2 }] },
                { "code": "D", "media_type": 8, "carousel_media": [{ "media_type": 1 }] }
            ],
            "more_available": true,
            "next_max_id": "3_4242",
            "status": "ok"
        })
        .to_string();
        let page = parse_feed(&body).unwrap();
        let videos: Vec<&str> = page.rows.iter().filter(|row| row.video).map(|row| row.code.as_str()).collect();
        assert_eq!(videos, ["A", "C"]);
        assert!(page.more);
        assert_eq!(page.next.as_deref(), Some("3_4242"));
    }

    #[test]
    fn the_videos_are_listed_as_a_playlist_is() {
        let profile = parse_profile(&profile_body()).unwrap();
        let videos: Vec<Row> = profile.rows.iter().filter(|row| row.video).cloned().collect();
        let list = listing("gezgin.ada", &profile, videos, vec![WARN_SIGN_IN.into()], "https://instagram.com/gezgin.ada");
        assert!(list.listing);
        assert_eq!(list.title, "Gezgin Ada (@gezgin.ada)");
        assert_eq!(list.canonical_url, "https://www.instagram.com/gezgin.ada/");
        assert_eq!(list.entry_count, Some(2));
        assert_eq!(list.entries[0].title, "Kapadokya'da bir sabah");
        assert_eq!(list.entries[0].upload_date.as_deref(), Some("20251113"));
        assert_eq!(list.entries[1].title, "Instagram V2");
        assert_eq!(list.tracks[1].url.as_deref(), Some("https://www.instagram.com/p/V2/"));
        assert_eq!(list.warnings, [WARN_SIGN_IN]);
    }

    #[test]
    fn a_long_caption_is_cut_to_a_title() {
        let row = Row {
            code: "X".into(),
            video: true,
            reel: false,
            caption: Some("ş".repeat(100)),
            thumbnail: None,
            duration: None,
            taken_at: None,
        };
        let title = title(&row);
        assert_eq!(title.chars().count(), 81);
        assert!(title.ends_with('…'));
    }
}
