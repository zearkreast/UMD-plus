//! System notifications.
//!
//! Deliberately quiet: only completion and failure, only when the user has left
//! them enabled, and never for routine state changes like a download starting.
//! Written in the interface's language, which the settings carry.

use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

use crate::model::{AppErrorInfo, HistoryEntry};
use crate::downloader::schedule::AfterQueue;
use crate::settings::Settings;

/// The interface's language is Turkish or English (`Settings::sanitize`).
fn in_turkish(settings: &Settings) -> bool {
    settings.language == "tr"
}

/// The words of every notification, in the interface's language.
fn words(settings: &Settings, english: &'static str, turkish: &'static str) -> &'static str {
    if in_turkish(settings) {
        turkish
    } else {
        english
    }
}

/// The line a followed list's notification says. Turkish takes no plural
/// after a number, so one and several differ in English alone.
fn found_line(settings: &Settings, count: usize) -> String {
    match (in_turkish(settings), count) {
        (true, n) => format!("{n} yeni video kuyruğa eklendi."),
        (false, 1) => "1 new video was queued.".to_string(),
        (false, n) => format!("{n} new videos were queued."),
    }
}

fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/'])
        .next()
        .unwrap_or(path)
        .to_string()
}

pub fn download_complete(app: &AppHandle, settings: &Settings, entry: &HistoryEntry) {
    if !settings.notifications_enabled || !settings.notify_on_complete {
        return;
    }

    let _ = app
        .notification()
        .builder()
        .title(words(settings, "Download complete", "İndirme tamamlandı"))
        .body(file_name(&entry.file_path))
        .show();
}

/// The queue has run out and the computer or the app is about to follow.
pub fn queue_ending(app: &AppHandle, settings: &Settings, after: AfterQueue) {
    if !settings.notifications_enabled {
        return;
    }
    let body = match after {
        AfterQueue::Nothing => return,
        AfterQueue::Quit => words(settings, "UMD+ closes in a minute.", "UMD+ bir dakika içinde kapanacak."),
        AfterQueue::Sleep => words(
            settings,
            "This computer goes to sleep in a minute.",
            "Bilgisayar bir dakika içinde uyku moduna geçecek.",
        ),
        AfterQueue::Shutdown => words(
            settings,
            "This computer shuts down in a minute.",
            "Bilgisayar bir dakika içinde kapanacak.",
        ),
    };
    let _ = app
        .notification()
        .builder()
        .title(words(settings, "Downloads finished", "İndirmeler bitti"))
        .body(body)
        .show();
}

/// New items of a followed list went into the queue.
pub fn subscription_found(app: &AppHandle, settings: &Settings, title: &str, count: usize) {
    if !settings.notifications_enabled || !settings.subscription_notify {
        return;
    }
    let _ = app.notification().builder().title(title).body(found_line(settings, count)).show();
}

pub fn download_failed(app: &AppHandle, settings: &Settings, title: &str, error: &AppErrorInfo) {
    if !settings.notifications_enabled || !settings.notify_on_error {
        return;
    }

    // The notification carries the same plain-language summary the UI shows,
    // never the technical detail.
    let _ = app
        .notification()
        .builder()
        .title(words(settings, "Download failed", "İndirme başarısız"))
        .body(format!("{title}\n{}", error.title))
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speaking(language: &str) -> Settings {
        Settings { language: language.into(), ..Settings::default() }
    }

    #[test]
    fn a_notification_speaks_the_interfaces_language() {
        assert_eq!(words(&speaking("tr"), "Download complete", "İndirme tamamlandı"), "İndirme tamamlandı");
        assert_eq!(words(&speaking("en"), "Download complete", "İndirme tamamlandı"), "Download complete");
    }

    #[test]
    fn new_videos_are_counted_in_either_language() {
        assert_eq!(found_line(&speaking("tr"), 1), "1 yeni video kuyruğa eklendi.");
        assert_eq!(found_line(&speaking("tr"), 3), "3 yeni video kuyruğa eklendi.");
        assert_eq!(found_line(&speaking("en"), 1), "1 new video was queued.");
        assert_eq!(found_line(&speaking("en"), 3), "3 new videos were queued.");
    }
}
