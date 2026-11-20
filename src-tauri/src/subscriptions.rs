//! Channels and lists the user follows.
//!
//! A subscription is a list the app can open -- a channel, a playlist, an
//! album -- and the download options it was followed with. Every few hours,
//! while the app is open or in the tray, each one is read again, and what it
//! holds that was never seen before goes into the queue. What was seen is
//! remembered by address, so a video taken down and put back up is not
//! downloaded twice, and a list that keeps its order oldest first works as
//! well as one that puts the newest on top.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Notify;

use crate::db::{platform_from_str, platform_to_str, Database};
use crate::downloader::queue::QueueManager;
use crate::error::{AppError, AppResult};
use crate::model::{DownloadMode, DownloadRequest, DownloadStatus, MediaMetadata, PlatformId, QualityPreference};
use crate::settings::Settings;
use crate::{log_info, log_warn, notify, providers, util};

pub const EVENT_CHANGED: &str = "subscriptions://changed";

/// The tables, made where they are missing. Run at every start by
/// `Database::migrate`, so an install from before them gains them.
pub const TABLES: &str = r#"
    CREATE TABLE IF NOT EXISTS subscriptions (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        url             TEXT    NOT NULL UNIQUE,
        title           TEXT    NOT NULL,
        platform        TEXT    NOT NULL,
        thumbnail_url   TEXT,
        enabled         INTEGER NOT NULL DEFAULT 1,
        skip_shorts     INTEGER NOT NULL DEFAULT 1,
        created_at      INTEGER NOT NULL,
        last_checked_at INTEGER,
        last_found      INTEGER NOT NULL DEFAULT 0,
        last_error      TEXT,
        failures        INTEGER NOT NULL DEFAULT 0,
        template_json   TEXT    NOT NULL
    );
    CREATE TABLE IF NOT EXISTS subscription_seen (
        subscription_id INTEGER NOT NULL,
        key             TEXT    NOT NULL,
        PRIMARY KEY (subscription_id, key)
    );
"#;

/// More new items than this in one reading is a list that changed its
/// addresses, not one that published that much at once: they are marked as
/// seen and the first of them alone are queued.
const MOST_NEW_AT_ONCE: usize = 20;

/// What counts as a Short where the address does not say so.
const SHORT_SEC: f64 = 60.0;

/// How long after the app starts the first look is taken, so a launch is not
/// spent reading lists.
const FIRST_LOOK: Duration = Duration::from_secs(20);

/// How often the loop wakes to see whether any subscription is due.
const TICK: Duration = Duration::from_secs(10 * 60);

/// How many items "the top of the list" is when following.
const TOP: usize = 5;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscription {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub platform: PlatformId,
    pub thumbnail_url: Option<String>,
    pub enabled: bool,
    pub skip_shorts: bool,
    pub created_at: i64,
    pub last_checked_at: Option<i64>,
    /// How many new items the latest reading queued.
    pub last_found: u32,
    /// Why the latest reading failed, once it has failed often enough to say.
    pub last_error: Option<String>,
    pub failures: u32,
    /// The options each new item is downloaded with; its address is the list's.
    pub template: DownloadRequest,
}

/// What is done with what a list already holds when it is followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Backfill {
    /// Only what comes after.
    None,
    /// The first few of the list as well.
    Top,
    All,
}

/// A change made on the Subscriptions screen.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SubscriptionChange {
    pub enabled: Option<bool>,
    pub skip_shorts: Option<bool>,
    pub mode: Option<DownloadMode>,
}

// -- what a reading holds ----------------------------------------------------

/// What an item is remembered by: its own address.
pub fn item_key(entry: &MediaMetadata) -> String {
    if entry.canonical_url.is_empty() {
        entry.url.clone()
    } else {
        entry.canonical_url.clone()
    }
}

/// A Short by its address, or by its length where the list gives one.
pub fn is_short(entry: &MediaMetadata) -> bool {
    let key = item_key(entry);
    key.contains("/shorts/") || entry.duration_sec.is_some_and(|length| length > 0.0 && length <= SHORT_SEC)
}

/// The items of `list` to queue, as the 1-based positions a request names
/// them by, and every key the reading leaves seen: the new items, and the
/// Shorts passed over, so neither is looked at again.
pub fn new_items(list: &MediaMetadata, seen: &HashSet<String>, skip_shorts: bool) -> (Vec<u32>, Vec<String>) {
    let mut positions = Vec::new();
    let mut keys = Vec::new();
    for (index, entry) in list.entries.iter().enumerate() {
        let key = item_key(entry);
        if key.is_empty() || seen.contains(&key) || keys.contains(&key) {
            continue;
        }
        if !(skip_shorts && is_short(entry)) {
            positions.push(index as u32 + 1);
        }
        keys.push(key);
    }
    positions.truncate(MOST_NEW_AT_ONCE);
    (positions, keys)
}

/// The items queued when a list is followed.
pub fn backfill_positions(list: &MediaMetadata, backfill: Backfill, skip_shorts: bool) -> Vec<u32> {
    let wanted: Vec<u32> = list
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| !(skip_shorts && is_short(entry)))
        .map(|(index, _)| index as u32 + 1)
        .collect();
    match backfill {
        Backfill::None => Vec::new(),
        Backfill::Top => wanted.into_iter().take(TOP).collect(),
        Backfill::All => wanted,
    }
}

/// Whether `subscription` is to be read again at `now`.
pub fn due(subscription: &Subscription, now: i64, interval_hours: u32) -> bool {
    let interval = i64::from(interval_hours.max(1)) * 3_600_000;
    subscription.enabled && subscription.last_checked_at.is_none_or(|at| now - at >= interval)
}

/// The options a subscription downloads with, in `mode`. A quality or a format
/// picked for one kind means nothing for the other, so they start over.
pub fn with_mode(mut template: DownloadRequest, mode: DownloadMode) -> DownloadRequest {
    if template.mode != mode {
        template.mode = mode;
        template.quality = QualityPreference::Best;
        template.container = None;
        template.video_format_id = None;
        template.audio_format_id = None;
    }
    template
}

// -- storage -------------------------------------------------------------------

impl Database {
    pub fn subscriptions(&self) -> AppResult<Vec<Subscription>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, url, title, platform, thumbnail_url, enabled, skip_shorts, created_at,
                    last_checked_at, last_found, last_error, failures, template_json
             FROM subscriptions ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], read_row)?;
        let mut found = Vec::new();
        for row in rows {
            // A row whose options no longer read is left out rather than
            // failing the whole list.
            if let Some(subscription) = row? {
                found.push(subscription);
            }
        }
        Ok(found)
    }

    pub fn subscription(&self, id: i64) -> AppResult<Option<Subscription>> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT id, url, title, platform, thumbnail_url, enabled, skip_shorts, created_at,
                        last_checked_at, last_found, last_error, failures, template_json
                 FROM subscriptions WHERE id = ?1",
                params![id],
                read_row,
            )
            .optional()?;
        Ok(row.flatten())
    }

    fn subscription_insert(&self, subscription: &Subscription, seen: &[String]) -> AppResult<i64> {
        let template = serde_json::to_string(&subscription.template)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let taken = tx
            .query_row("SELECT 1 FROM subscriptions WHERE url = ?1", params![subscription.url], |_| Ok(()))
            .optional()?
            .is_some();
        if taken {
            return Err(AppError::Other("this list is already followed".into()));
        }
        tx.execute(
            "INSERT INTO subscriptions
                (url, title, platform, thumbnail_url, enabled, skip_shorts, created_at,
                 last_checked_at, last_found, template_json)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?6, 0, ?7)",
            params![
                subscription.url,
                subscription.title,
                platform_to_str(subscription.platform),
                subscription.thumbnail_url,
                subscription.skip_shorts,
                subscription.created_at,
                template,
            ],
        )?;
        let id = tx.last_insert_rowid();
        mark_seen(&tx, id, seen)?;
        tx.commit()?;
        Ok(id)
    }

    fn subscription_save(&self, subscription: &Subscription) -> AppResult<()> {
        let template = serde_json::to_string(&subscription.template)?;
        self.lock()?.execute(
            "UPDATE subscriptions SET enabled = ?2, skip_shorts = ?3, template_json = ?4 WHERE id = ?1",
            params![subscription.id, subscription.enabled, subscription.skip_shorts, template],
        )?;
        Ok(())
    }

    pub fn subscription_delete(&self, id: i64) -> AppResult<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM subscription_seen WHERE subscription_id = ?1", params![id])?;
        tx.execute("DELETE FROM subscriptions WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    fn seen_keys(&self, id: i64) -> AppResult<HashSet<String>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT key FROM subscription_seen WHERE subscription_id = ?1")?;
        let keys = stmt.query_map(params![id], |row| row.get::<_, String>(0))?;
        keys.collect::<Result<HashSet<_>, _>>().map_err(Into::into)
    }

    /// Record a reading: what it saw, what it queued, or why it failed.
    fn record_reading(&self, id: i64, at: i64, seen: &[String], outcome: Result<u32, &str>) -> AppResult<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        match outcome {
            Ok(found) => {
                mark_seen(&tx, id, seen)?;
                tx.execute(
                    "UPDATE subscriptions SET last_checked_at = ?2, last_found = ?3, last_error = NULL, failures = 0
                     WHERE id = ?1",
                    params![id, at, found],
                )?;
            }
            Err(reason) => {
                tx.execute(
                    "UPDATE subscriptions SET last_checked_at = ?2, last_error = ?3, failures = failures + 1
                     WHERE id = ?1",
                    params![id, at, reason],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

fn mark_seen(tx: &rusqlite::Transaction<'_>, id: i64, keys: &[String]) -> AppResult<()> {
    let mut stmt = tx.prepare("INSERT OR IGNORE INTO subscription_seen (subscription_id, key) VALUES (?1, ?2)")?;
    for key in keys {
        stmt.execute(params![id, key])?;
    }
    Ok(())
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Subscription>> {
    let template: String = row.get(12)?;
    let Ok(template) = serde_json::from_str::<DownloadRequest>(&template) else {
        return Ok(None);
    };
    Ok(Some(Subscription {
        id: row.get(0)?,
        url: row.get(1)?,
        title: row.get(2)?,
        platform: platform_from_str(&row.get::<_, String>(3)?),
        thumbnail_url: row.get(4)?,
        enabled: row.get(5)?,
        skip_shorts: row.get(6)?,
        created_at: row.get(7)?,
        last_checked_at: row.get(8)?,
        last_found: row.get(9)?,
        last_error: row.get(10)?,
        failures: row.get(11)?,
        template,
    }))
}

// -- the reader ------------------------------------------------------------------

pub struct Subscriptions {
    app: AppHandle,
    db: Arc<Database>,
    settings: Arc<Mutex<Settings>>,
    queue: Arc<QueueManager>,
    wake: Notify,
    /// The next reading takes every subscription, due or not.
    everything: AtomicBool,
}

impl Subscriptions {
    pub fn new(
        app: AppHandle,
        db: Arc<Database>,
        settings: Arc<Mutex<Settings>>,
        queue: Arc<QueueManager>,
    ) -> Arc<Self> {
        Arc::new(Self {
            app,
            db,
            settings,
            queue,
            wake: Notify::new(),
            everything: AtomicBool::new(false),
        })
    }

    fn settings(&self) -> Settings {
        self.settings.lock().map(|guard| guard.clone()).unwrap_or_default()
    }

    /// Read whatever is due, a moment after start and then whenever the loop
    /// wakes. A launch after a while closed reads what was missed, since its
    /// last reading is older than the interval.
    pub fn spawn(self: &Arc<Self>) {
        let reader = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(FIRST_LOOK).await;
            loop {
                let everything = reader.everything.swap(false, Ordering::AcqRel);
                reader.read_due(everything).await;
                tokio::select! {
                    _ = reader.wake.notified() => {}
                    _ = tokio::time::sleep(TICK) => {}
                }
            }
        });
    }

    /// Read every subscription now, due or not.
    pub fn read_now(&self) {
        self.everything.store(true, Ordering::Release);
        self.wake.notify_one();
    }

    fn changed(&self) {
        let _ = self.app.emit(EVENT_CHANGED, ());
    }

    async fn read_due(&self, everything: bool) {
        let settings = self.settings();
        let Ok(all) = self.db.subscriptions() else { return };
        let now = util::now_ms();
        let due: Vec<Subscription> = all
            .into_iter()
            .filter(|subscription| {
                subscription.enabled && (everything || due(subscription, now, settings.subscription_interval_hours))
            })
            .collect();
        for subscription in due {
            self.read_one(&subscription, &settings).await;
            self.changed();
        }
    }

    async fn read_one(&self, subscription: &Subscription, settings: &Settings) {
        let now = util::now_ms();
        let list = match providers::analyze_newest(&subscription.url, settings).await {
            Ok(list) => list,
            Err(err) => {
                log_warn!("subscriptions", "{} could not be read: {err}", subscription.url);
                let _ = self.db.record_reading(subscription.id, now, &[], Err(&err.to_info().title));
                return;
            }
        };
        let seen = self.db.seen_keys(subscription.id).unwrap_or_default();
        let (positions, keys) = new_items(&list, &seen, subscription.skip_shorts);
        let queued = self.queue_items(&list, subscription, &positions, settings);
        if let Err(err) = self.db.record_reading(subscription.id, now, &keys, Ok(queued as u32)) {
            log_warn!("subscriptions", "the reading of {} was not kept: {err}", subscription.url);
        }
        if queued > 0 {
            log_info!("subscriptions", "{}: {queued} new", subscription.url);
            notify::subscription_found(&self.app, settings, &subscription.title, queued);
        }
    }

    /// Queue the items of `list` at `positions` with the subscription's
    /// options. Held paused when new items are not to start by themselves.
    fn queue_items(&self, list: &MediaMetadata, subscription: &Subscription, positions: &[u32], settings: &Settings) -> usize {
        if positions.is_empty() {
            return 0;
        }
        let mut template = subscription.template.clone();
        template.url = subscription.url.clone();
        let requests = match providers::gallery_requests(list, &template, Some(positions)) {
            Ok(requests) => requests,
            Err(err) => {
                log_warn!("subscriptions", "{} could not be queued: {err}", subscription.url);
                return 0;
            }
        };
        let created_at = util::now_ms();
        let status = if settings.subscription_auto_download {
            DownloadStatus::Queued
        } else {
            DownloadStatus::Paused
        };
        for request in &requests {
            self.queue.enqueue_with(request.clone(), created_at, status);
        }
        requests.len()
    }

    /// Follow the list `template` names, with its options.
    pub async fn follow(&self, template: DownloadRequest, backfill: Backfill, skip_shorts: bool) -> AppResult<Subscription> {
        let settings = self.settings();
        let list = match providers::recent_analysis(&template.url, &settings) {
            Some(list) => list,
            None => providers::analyze(&template.url, &settings).await?,
        };
        if !list.listing {
            return Err(AppError::Other("only a channel or a list can be followed".into()));
        }
        let url = if list.canonical_url.is_empty() { template.url.clone() } else { list.canonical_url.clone() };
        let mut subscription = Subscription {
            id: 0,
            url: url.clone(),
            title: list.title.clone(),
            platform: list.platform,
            thumbnail_url: list.thumbnail_url.clone(),
            enabled: true,
            skip_shorts,
            created_at: util::now_ms(),
            last_checked_at: None,
            last_found: 0,
            last_error: None,
            failures: 0,
            template: DownloadRequest { url, source: None, entry: None, ..template },
        };
        let seen: Vec<String> = list.entries.iter().map(item_key).filter(|key| !key.is_empty()).collect();
        subscription.id = self.db.subscription_insert(&subscription, &seen)?;
        let positions = backfill_positions(&list, backfill, skip_shorts);
        self.queue_items(&list, &subscription, &positions, &settings);
        self.changed();
        self.db.subscription(subscription.id)?.ok_or_else(|| AppError::Other("the subscription was not kept".into()))
    }

    pub fn change(&self, id: i64, change: SubscriptionChange) -> AppResult<Subscription> {
        let mut subscription = self
            .db
            .subscription(id)?
            .ok_or_else(|| AppError::NotFound { status: 404, detail: format!("no subscription {id}") })?;
        if let Some(enabled) = change.enabled {
            subscription.enabled = enabled;
        }
        if let Some(skip) = change.skip_shorts {
            subscription.skip_shorts = skip;
        }
        if let Some(mode) = change.mode {
            subscription.template = with_mode(subscription.template, mode);
        }
        self.db.subscription_save(&subscription)?;
        self.changed();
        Ok(subscription)
    }

    pub fn forget(&self, id: i64) -> AppResult<()> {
        self.db.subscription_delete(id)?;
        self.changed();
        Ok(())
    }
}

// -- commands --------------------------------------------------------------------

#[tauri::command]
pub fn list_subscriptions(state: State<'_, Arc<Subscriptions>>) -> AppResult<Vec<Subscription>> {
    state.db.subscriptions()
}

#[tauri::command]
pub async fn add_subscription(
    state: State<'_, Arc<Subscriptions>>,
    request: DownloadRequest,
    backfill: Backfill,
    skip_shorts: bool,
) -> AppResult<Subscription> {
    state.follow(request, backfill, skip_shorts).await
}

#[tauri::command]
pub fn update_subscription(
    state: State<'_, Arc<Subscriptions>>,
    id: i64,
    change: SubscriptionChange,
) -> AppResult<Subscription> {
    state.change(id, change)
}

#[tauri::command]
pub fn remove_subscription(state: State<'_, Arc<Subscriptions>>, id: i64) -> AppResult<()> {
    state.forget(id)
}

#[tauri::command]
pub fn check_subscriptions(state: State<'_, Arc<Subscriptions>>) {
    state.read_now();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(url: &str, length: Option<f64>) -> MediaMetadata {
        let mut entry = providers::tests_support::blank();
        entry.url = url.into();
        entry.canonical_url = url.into();
        entry.duration_sec = length;
        entry
    }

    fn list(urls: &[(&str, Option<f64>)]) -> MediaMetadata {
        let mut list = providers::tests_support::blank();
        list.listing = true;
        list.entries = urls.iter().map(|(url, length)| item(url, *length)).collect();
        list
    }

    #[test]
    fn what_was_not_seen_is_new_wherever_it_stands() {
        let reading = list(&[
            ("https://www.youtube.com/watch?v=new1", Some(600.0)),
            ("https://www.youtube.com/watch?v=old1", Some(600.0)),
            ("https://www.youtube.com/watch?v=new2", None),
        ]);
        let seen: HashSet<String> = ["https://www.youtube.com/watch?v=old1".to_string()].into();
        let (positions, keys) = new_items(&reading, &seen, true);
        assert_eq!(positions, [1, 3]);
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn a_short_is_passed_over_and_not_looked_at_again() {
        let reading = list(&[
            ("https://www.youtube.com/shorts/abc", None),
            ("https://www.youtube.com/watch?v=brief", Some(45.0)),
            ("https://www.youtube.com/watch?v=long", Some(900.0)),
        ]);
        let (positions, keys) = new_items(&reading, &HashSet::new(), true);
        assert_eq!(positions, [3]);
        assert_eq!(keys.len(), 3, "the Shorts are seen too");

        let (all, _) = new_items(&reading, &HashSet::new(), false);
        assert_eq!(all, [1, 2, 3]);
    }

    #[test]
    fn a_list_that_changed_its_addresses_does_not_flood_the_queue() {
        let urls: Vec<String> = (0..50).map(|n| format!("https://example.com/v/{n}")).collect();
        let reading = list(&urls.iter().map(|url| (url.as_str(), Some(600.0))).collect::<Vec<_>>());
        let (positions, keys) = new_items(&reading, &HashSet::new(), false);
        assert_eq!(positions.len(), MOST_NEW_AT_ONCE);
        assert_eq!(keys.len(), 50, "all of them are seen");
    }

    #[test]
    fn following_queues_none_the_top_or_all() {
        let reading = list(&[
            ("https://www.youtube.com/shorts/s", None),
            ("https://example.com/1", Some(600.0)),
            ("https://example.com/2", Some(600.0)),
            ("https://example.com/3", Some(600.0)),
            ("https://example.com/4", Some(600.0)),
            ("https://example.com/5", Some(600.0)),
            ("https://example.com/6", Some(600.0)),
        ]);
        assert!(backfill_positions(&reading, Backfill::None, true).is_empty());
        assert_eq!(backfill_positions(&reading, Backfill::Top, true), [2, 3, 4, 5, 6]);
        assert_eq!(backfill_positions(&reading, Backfill::All, true).len(), 6);
        assert_eq!(backfill_positions(&reading, Backfill::All, false).len(), 7);
    }

    #[test]
    fn a_subscription_is_read_again_once_its_interval_has_passed() {
        let template: DownloadRequest = serde_json::from_value(serde_json::json!({
            "url": "https://www.youtube.com/@x/videos",
            "mode": "video",
            "quality": { "type": "best" },
            "videoFormatId": null,
            "audioFormatId": null,
            "container": "mp4",
            "watermark": "any",
            "outputDir": null,
            "title": null,
            "thumbnailUrl": null,
            "platform": null
        }))
        .unwrap();
        let hour = 3_600_000;
        let mut subscription = Subscription {
            id: 1,
            url: template.url.clone(),
            title: "x".into(),
            platform: PlatformId::Youtube,
            thumbnail_url: None,
            enabled: true,
            skip_shorts: true,
            created_at: 0,
            last_checked_at: Some(10 * hour),
            last_found: 0,
            last_error: None,
            failures: 0,
            template: template.clone(),
        };
        assert!(!due(&subscription, 15 * hour, 6));
        assert!(due(&subscription, 16 * hour, 6));
        subscription.enabled = false;
        assert!(!due(&subscription, 99 * hour, 6));

        let audio = with_mode(template, DownloadMode::Audio);
        assert_eq!(audio.mode, DownloadMode::Audio);
        assert_eq!(audio.container, None);
    }

    #[test]
    fn a_followed_list_and_what_it_has_seen_are_kept() {
        let dir = std::env::temp_dir().join(format!("ud-subscriptions-{}", util::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(&dir.join("library.db")).unwrap();
        let template: DownloadRequest = serde_json::from_value(serde_json::json!({
            "url": "https://www.youtube.com/@x/videos", "mode": "video", "quality": { "type": "best" },
            "videoFormatId": null, "audioFormatId": null, "container": null, "watermark": "any",
            "outputDir": null, "title": null, "thumbnailUrl": null, "platform": null
        }))
        .unwrap();
        let subscription = Subscription {
            id: 0,
            url: template.url.clone(),
            title: "Yol Notları".into(),
            platform: PlatformId::Youtube,
            thumbnail_url: None,
            enabled: true,
            skip_shorts: true,
            created_at: 5,
            last_checked_at: None,
            last_found: 0,
            last_error: None,
            failures: 0,
            template,
        };
        let id = db.subscription_insert(&subscription, &["a".into(), "b".into()]).unwrap();
        assert!(db.subscription_insert(&subscription, &[]).is_err(), "followed once");
        assert_eq!(db.seen_keys(id).unwrap().len(), 2);

        db.record_reading(id, 99, &["c".into()], Ok(1)).unwrap();
        db.record_reading(id, 100, &[], Err("offline")).unwrap();
        let kept = db.subscription(id).unwrap().unwrap();
        assert_eq!((kept.last_checked_at, kept.last_found, kept.failures), (Some(100), 1, 1));
        assert_eq!(kept.last_error.as_deref(), Some("offline"));
        assert_eq!(db.seen_keys(id).unwrap().len(), 3);

        db.subscription_delete(id).unwrap();
        assert!(db.subscriptions().unwrap().is_empty());
        assert!(db.seen_keys(id).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
