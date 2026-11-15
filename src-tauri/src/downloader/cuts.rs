//! What is taken out of a finished download, and what it is cut into.
//!
//! Both work on the file the transfer produced, after it, so the transfer
//! itself stays the app's own fast one whichever way it was fetched. Both are
//! stream copies: nothing is encoded again, and a cut lands on the nearest
//! keyframe rather than on the exact frame.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::downloader::control::TaskControl;
use crate::error::{AppError, AppResult};
use crate::media::ffmpeg;
use crate::model::Chapter;
use crate::settings::Settings;
use crate::{filename, log_info};

/// The categories taken out: paid promotion, the creator's own plugs, and the
/// "like and subscribe" requests. An intro or an outro is part of the video.
const CATEGORIES: &str = r#"["sponsor","selfpromo","interaction"]"#;

/// A stretch shorter than this is not worth a cut that lands on a keyframe
/// anyway.
const SHORTEST_CUT: f64 = 1.0;

/// The video id of a YouTube link, which is what SponsorBlock is asked by.
pub fn youtube_id(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?.trim_start_matches("www.").trim_start_matches("m.");
    let id = match host {
        "youtu.be" => parsed.path_segments()?.next().map(str::to_string),
        "youtube.com" | "music.youtube.com" => {
            let mut path = parsed.path_segments()?;
            match path.next() {
                Some("watch") => parsed.query_pairs().find(|(key, _)| key == "v").map(|(_, value)| value.into_owned()),
                Some("shorts" | "live" | "embed") => path.next().map(str::to_string),
                _ => None,
            }
        }
        _ => None,
    }?;
    let valid = id.len() == 11 && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    valid.then_some(id)
}

/// The stretches to take out of `id`, as SponsorBlock's community has marked
/// them. `None` when there are none or the answer could not be had: a video is
/// never held back for this.
pub async fn sponsor_segments(id: &str, settings: &Settings) -> Option<Vec<(f64, f64)>> {
    let client = crate::net::client(settings).ok()?;
    let response = client
        .get("https://sponsor.ajay.app/api/skipSegments")
        .query(&[("videoID", id), ("categories", CATEGORIES)])
        .send()
        .await
        .ok()?;
    // 404 is the service's answer for a video nobody has marked.
    if !response.status().is_success() {
        return None;
    }
    let found: Vec<serde_json::Value> = response.json().await.ok()?;
    let segments: Vec<(f64, f64)> = found
        .iter()
        .filter_map(|entry| {
            let pair = entry.get("segment")?.as_array()?;
            Some((pair.first()?.as_f64()?, pair.get(1)?.as_f64()?))
        })
        .collect();
    (!segments.is_empty()).then_some(segments)
}

/// What is left of `0..length` once `removed` is taken out, in order.
/// Overlapping and touching stretches are taken out as one, and one too short
/// to be worth a cut is left in. Empty when nothing is taken out.
pub fn kept_ranges(removed: &[(f64, f64)], length: f64) -> Vec<(f64, f64)> {
    let mut cuts: Vec<(f64, f64)> = removed
        .iter()
        .map(|&(start, end)| (start.max(0.0), end.min(length)))
        .filter(|(start, end)| end - start >= SHORTEST_CUT)
        .collect();
    if cuts.is_empty() {
        return Vec::new();
    }
    cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for cut in cuts {
        match merged.last_mut() {
            Some(last) if cut.0 <= last.1 => last.1 = last.1.max(cut.1),
            _ => merged.push(cut),
        }
    }

    let mut kept = Vec::new();
    let mut from = 0.0;
    for (start, end) in merged {
        if start - from > 0.05 {
            kept.push((from, start));
        }
        from = end;
    }
    if length - from > 0.05 {
        kept.push((from, length));
    }
    kept
}

/// `chapters` on the shortened timeline `kept` makes. A chapter that started
/// inside a part taken out now starts where that part was; one left with
/// nothing of its own is dropped.
pub fn shift_chapters(chapters: &[Chapter], kept: &[(f64, f64)]) -> Vec<Chapter> {
    let moved = |at: f64| -> f64 {
        let mut shifted = 0.0;
        for &(start, end) in kept {
            if at < start {
                return shifted;
            }
            if at <= end {
                return shifted + (at - start);
            }
            shifted += end - start;
        }
        shifted
    };
    chapters
        .iter()
        .map(|chapter| Chapter {
            start_sec: moved(chapter.start_sec),
            end_sec: moved(chapter.end_sec),
            title: chapter.title.clone(),
        })
        .filter(|chapter| chapter.end_sec - chapter.start_sec >= SHORTEST_CUT)
        .collect()
}

/// The concat demuxer's list of the parts of `input` to keep.
pub fn concat_list(input: &Path, kept: &[(f64, f64)]) -> String {
    // The demuxer's own quoting: a single quote closes, and one inside is
    // written as a quote that is closed, escaped and opened again.
    let quoted = input.to_string_lossy().replace('\\', "/").replace('\'', r"'\''");
    let mut list = String::from("ffconcat version 1.0\n");
    for &(start, end) in kept {
        list.push_str(&format!("file '{quoted}'\ninpoint {start:.3}\noutpoint {end:.3}\n"));
    }
    list
}

/// Take the stretches SponsorBlock marks out of `produced`, a YouTube download
/// of `length` seconds. Gives back the file to carry on with and the parts of
/// the original it holds, which are empty when nothing was taken out. Anything
/// short of a cancel keeps the download whole.
pub async fn take_out_sponsors(
    task_id: &str,
    produced: PathBuf,
    id: &str,
    length: f64,
    settings: &Settings,
    control: &Arc<TaskControl>,
    temp_dir: &Path,
) -> AppResult<(PathBuf, Vec<(f64, f64)>)> {
    let Some(segments) = sponsor_segments(id, settings).await else {
        return Ok((produced, Vec::new()));
    };
    let kept = kept_ranges(&segments, length);
    if kept.is_empty() || crate::tools::ffmpeg_path().is_none() {
        return Ok((produced, Vec::new()));
    }

    let extension = produced.extension().and_then(|ext| ext.to_str()).unwrap_or("mp4").to_string();
    let list = temp_dir.join(format!("{task_id}.cuts.txt"));
    let output = ffmpeg::intermediate_path(temp_dir, task_id, "cut", &extension);
    if let Err(err) = std::fs::write(&list, concat_list(&produced, &kept)) {
        log_info!("downloader", "task {task_id}: the sponsor cuts could not be listed: {err}");
        return Ok((produced, Vec::new()));
    }
    let left: f64 = kept.iter().map(|(start, end)| end - start).sum();
    let result = ffmpeg::run_with_progress(&ffmpeg::join_parts_args(&list, &output), Some(left), Arc::clone(control), &mut |_| {}).await;
    let _ = std::fs::remove_file(&list);

    match result {
        Ok(()) => {
            log_info!(
                "downloader",
                "task {task_id}: took {:.0} s of sponsored parts out",
                length - left
            );
            let _ = std::fs::remove_file(&produced);
            Ok((output, kept))
        }
        Err(AppError::Canceled) => {
            let _ = std::fs::remove_file(&output);
            Err(AppError::Canceled)
        }
        Err(err) => {
            log_info!("downloader", "task {task_id}: the sponsored parts could not be taken out: {err}");
            let _ = std::fs::remove_file(&output);
            Ok((produced, Vec::new()))
        }
    }
}

/// Write each of `chapters` of `file` as a file of its own, numbered, in a
/// folder beside it named after it. The whole file stays where it is. Says how
/// many were written.
pub async fn split_into_chapters(
    task_id: &str,
    file: &Path,
    chapters: &[Chapter],
    control: &Arc<TaskControl>,
) -> AppResult<usize> {
    if chapters.len() < 2 || crate::tools::ffmpeg_path().is_none() {
        return Ok(0);
    }
    let extension = file.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_string();
    let folder = file.with_extension("");
    if let Err(err) = std::fs::create_dir_all(&folder) {
        log_info!("downloader", "task {task_id}: no folder for the chapters: {err}");
        return Ok(0);
    }

    let mut written = 0;
    for (index, chapter) in chapters.iter().enumerate() {
        let title = filename::truncate_stem(&filename::sanitize_component(&chapter.title), 80);
        let stem = if title.is_empty() {
            format!("{:02}", index + 1)
        } else {
            format!("{:02} - {title}", index + 1)
        };
        let output = filename::unique_path(&folder, &stem, &extension);
        let length = chapter.end_sec - chapter.start_sec;
        let args = ffmpeg::excerpt_args(file, &output, chapter.start_sec, length);
        match ffmpeg::run_with_progress(&args, Some(length), Arc::clone(control), &mut |_| {}).await {
            Ok(()) => written += 1,
            Err(AppError::Canceled) => {
                let _ = std::fs::remove_file(&output);
                return Err(AppError::Canceled);
            }
            Err(err) => {
                log_info!("downloader", "task {task_id}: chapter {} could not be written: {err}", index + 1);
                let _ = std::fs::remove_file(&output);
            }
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_video_id_is_read_from_every_kind_of_youtube_link() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10",
            "https://youtu.be/dQw4w9WgXcQ?si=x",
            "https://m.youtube.com/shorts/dQw4w9WgXcQ",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ",
        ] {
            assert_eq!(youtube_id(url).as_deref(), Some("dQw4w9WgXcQ"), "{url}");
        }
        assert_eq!(youtube_id("https://vimeo.com/123456"), None);
        assert_eq!(youtube_id("https://www.youtube.com/watch?v=short"), None);
        assert_eq!(youtube_id("https://www.youtube.com/@channel"), None);
    }

    #[test]
    fn what_is_kept_is_the_rest_of_the_video() {
        assert_eq!(kept_ranges(&[(10.0, 20.0)], 60.0), [(0.0, 10.0), (20.0, 60.0)]);
        // Overlapping and touching parts go as one; a sliver stays.
        assert_eq!(
            kept_ranges(&[(30.0, 40.0), (10.0, 20.0), (18.0, 25.0), (50.0, 50.4)], 60.0),
            [(0.0, 10.0), (25.0, 30.0), (40.0, 60.0)]
        );
        // A part at either end leaves no empty stretch behind.
        assert_eq!(kept_ranges(&[(0.0, 5.0), (55.0, 70.0)], 60.0), [(5.0, 55.0)]);
        assert!(kept_ranges(&[], 60.0).is_empty());
        assert!(kept_ranges(&[(3.0, 3.5)], 60.0).is_empty(), "nothing worth a cut");
    }

    #[test]
    fn chapters_follow_the_parts_that_were_taken_out() {
        let chapter = |start: f64, end: f64, title: &str| Chapter { start_sec: start, end_sec: end, title: title.into() };
        let chapters = [chapter(0.0, 30.0, "Intro"), chapter(30.0, 40.0, "Sponsor"), chapter(40.0, 100.0, "Talk")];
        let kept = [(0.0, 30.0), (40.0, 100.0)];
        assert_eq!(shift_chapters(&chapters, &kept), [chapter(0.0, 30.0, "Intro"), chapter(30.0, 90.0, "Talk")]);
    }

    #[test]
    fn the_cut_list_names_the_file_once_for_each_part() {
        let list = concat_list(Path::new(r"C:\temp\it's.mp4"), &[(0.0, 10.0), (20.5, 60.0)]);
        assert_eq!(
            list,
            "ffconcat version 1.0\n\
             file 'C:/temp/it'\\''s.mp4'\ninpoint 0.000\noutpoint 10.000\n\
             file 'C:/temp/it'\\''s.mp4'\ninpoint 20.500\noutpoint 60.000\n"
        );
    }
}
