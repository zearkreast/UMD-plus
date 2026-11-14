//! File name construction.
//!
//! Two jobs: expand the user's template, and make the result something Windows
//! will actually accept. Titles arrive from third-party sources, so they can
//! contain path separators, reserved device names, emoji, RTL marks and a few
//! hundred characters of clickbait -- all of which are handled here rather than
//! anywhere near a shell.

use std::path::{Path, PathBuf};

/// Characters Windows rejects outright in a file name.
const INVALID: [char; 9] = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

/// Device names that cannot be used as a file stem on Windows, with or without
/// an extension.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Leaves room inside the classic 260-char path limit for the directory, the
/// extension, a " (n)" disambiguator and the trailing NUL.
const MAX_STEM_CHARS: usize = 120;

/// Strip everything a file system would refuse, then tidy the result.
pub fn sanitize_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());

    for ch in input.chars() {
        if INVALID.contains(&ch) {
            out.push('-');
        } else if (ch as u32) < 0x20 || ch == '\u{7f}' {
            // Control characters: drop silently.
            continue;
        } else if matches!(ch, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}') {
            // Bidirectional overrides can make a name display as something
            // other than what it is. Not useful in a file name.
            continue;
        } else {
            out.push(ch);
        }
    }

    // Collapse runs of whitespace, then trim what Windows would silently strip
    // from the end (trailing dots and spaces are not preserved).
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches(|c: char| c == '.' || c.is_whitespace());

    if trimmed.is_empty() {
        return String::new();
    }

    let upper = trimmed.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or(&upper);
    if RESERVED.contains(&stem) {
        return format!("_{trimmed}");
    }

    trimmed.to_string()
}

/// Truncate on a character boundary, not a byte one, and avoid ending on a
/// partial word when there is a nearby space to cut at.
pub fn truncate_stem(stem: &str, max_chars: usize) -> String {
    if stem.chars().count() <= max_chars {
        return stem.to_string();
    }

    // Counted in characters throughout. A space's place in bytes is two or
    // three times its place in characters in a Cyrillic or Japanese title,
    // and measured that way an early space passed for a late one.
    let kept: Vec<char> = stem.chars().take(max_chars).collect();
    let cut = kept
        .iter()
        .rposition(|ch| *ch == ' ')
        .filter(|index| *index > max_chars * 2 / 3)
        .unwrap_or(kept.len());

    let truncated: String = kept[..cut].iter().collect();
    truncated.trim_end_matches(['.', ' ', '-']).to_string()
}

pub struct NameContext<'a> {
    pub title: &'a str,
    pub creator: Option<&'a str>,
    pub quality: &'a str,
    pub platform: &'a str,
    /// ISO date, `YYYY-MM-DD`.
    pub date: &'a str,
    pub ext: &'a str,
}

/// The file's name as `template` gives it: its last part, where the parts
/// before a slash are folders (see [`render_folders`]).
pub fn render_template(template: &str, ctx: &NameContext<'_>) -> String {
    let name = template.rsplit(SEPARATORS).next().unwrap_or(template);
    post_process(expand(without_extension(name), ctx), ctx)
}

/// A slash in the template divides folders from the file's name, either way
/// round, since a Windows user writes the one they see in a path.
const SEPARATORS: [char; 2] = ['/', '\\'];

/// How long a folder's name may be.
const MAX_FOLDER_CHARS: usize = 80;

/// The folders `template` puts the file in, outermost first: every part
/// before its last slash, with its tokens filled in. Each is made safe as a
/// name on its own, so a slash in a title cannot open a folder and a part
/// that comes to nothing -- or to dots, which would climb out -- is left out.
pub fn render_folders(template: &str, ctx: &NameContext<'_>) -> Vec<String> {
    let mut parts: Vec<&str> = template.split(SEPARATORS).collect();
    parts.pop();
    parts
        .into_iter()
        .map(|part| truncate_stem(&tidy(expand(part, ctx)), MAX_FOLDER_CHARS))
        .filter(|folder| !folder.is_empty())
        .collect()
}

/// Expand `{token}` placeholders. Unknown tokens are left as-is so a typo is
/// visible in the preview rather than silently swallowed.
fn expand(template: &str, ctx: &NameContext<'_>) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;

    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };

        let token = &after[..end];
        match token {
            "title" => out.push_str(ctx.title),
            "creator" => out.push_str(ctx.creator.unwrap_or("")),
            "quality" => out.push_str(ctx.quality),
            "platform" => out.push_str(ctx.platform),
            "date" => out.push_str(ctx.date),
            "ext" => out.push_str(ctx.ext),
            unknown => {
                out.push('{');
                out.push_str(unknown);
                out.push('}');
            }
        }
        rest = &after[end + 1..];
    }

    out.push_str(rest);
    out
}

/// The template without an extension written at its end. The extension is
/// always added to the name, so "{title}.{ext}" would otherwise come out as
/// "Video.mp4.mp4".
fn without_extension(template: &str) -> &str {
    let trimmed = template.trim_end();
    match trimmed.strip_suffix("{ext}") {
        Some(rest) => rest.strip_suffix('.').unwrap_or(rest),
        None => template,
    }
}

/// Clean up the seams left when an optional token expanded to nothing, e.g.
/// "{creator} - {title}" with no creator would otherwise start with " - ".
fn post_process(rendered: String, ctx: &NameContext<'_>) -> String {
    let cleaned = tidy(rendered);
    if cleaned.is_empty() {
        sanitize_component(ctx.title)
    } else {
        cleaned
    }
}

/// A rendered part with its seams closed and nothing in it a file system
/// would refuse.
fn tidy(rendered: String) -> String {
    let mut value = rendered;
    for separator in [" - ", " – ", " | ", " · "] {
        while value.starts_with(separator) {
            value = value[separator.len()..].to_string();
        }
        while value.ends_with(separator) {
            value.truncate(value.len() - separator.len());
        }
        // Collapse a doubled separator left by a middle token vanishing.
        let doubled = format!("{separator}{separator}");
        while value.contains(&doubled) {
            value = value.replace(&doubled, separator);
        }
    }
    value = value.replace("[]", "").replace("()", "");

    sanitize_component(&value)
}

/// Build the final path, expanding the template, sanitising it, keeping it
/// inside the length limit and making it unique in the target directory.
pub fn build_output_path(
    dir: &Path,
    template: &str,
    ctx: &NameContext<'_>,
) -> PathBuf {
    let mut stem = render_template(template, ctx);
    if stem.is_empty() {
        stem = "download".to_string();
    }

    // Leave headroom so `dir + stem + ext` stays inside the legacy path limit
    // even on a deeply nested download folder.
    let dir_len = dir.as_os_str().len();
    let budget = MAX_STEM_CHARS.min(240usize.saturating_sub(dir_len + ctx.ext.len() + 8));
    stem = truncate_stem(&stem, budget.max(16));

    let ext = ctx.ext.trim_start_matches('.');
    unique_path(dir, &stem, ext)
}

/// `name.mp4`, then `name (2).mp4`, `name (3).mp4`, ...
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(if ext.is_empty() {
        stem.to_string()
    } else {
        format!("{stem}.{ext}")
    });
    if !first.exists() {
        return first;
    }

    for index in 2..10_000u32 {
        let candidate = dir.join(if ext.is_empty() {
            format!("{stem} ({index})")
        } else {
            format!("{stem} ({index}).{ext}")
        });
        if !candidate.exists() {
            return candidate;
        }
    }

    // Pathological case: fall back to a timestamped name rather than looping.
    dir.join(format!("{stem} ({}).{ext}", crate::util::now_ms()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>() -> NameContext<'a> {
        NameContext {
            title: "My Video",
            creator: Some("creator"),
            quality: "1080p",
            platform: "youtube",
            date: "2026-09-07",
            ext: "mp4",
        }
    }

    #[test]
    fn strips_windows_reserved_characters() {
        assert_eq!(sanitize_component(r#"a/b\c:d*e?f"g<h>i|j"#), "a-b-c-d-e-f-g-h-i-j");
    }

    #[test]
    fn drops_control_and_bidi_characters() {
        assert_eq!(sanitize_component("clean\u{202e}nes\u{0007}s"), "cleanness");
    }

    #[test]
    fn trims_trailing_dots_and_spaces() {
        assert_eq!(sanitize_component("name...  "), "name");
        assert_eq!(sanitize_component("  spaced  out  "), "spaced out");
    }

    #[test]
    fn escapes_reserved_device_names() {
        assert_eq!(sanitize_component("CON"), "_CON");
        assert_eq!(sanitize_component("nul.txt"), "_nul.txt");
        assert_eq!(sanitize_component("console"), "console");
    }

    #[test]
    fn empty_after_sanitizing_returns_empty() {
        assert_eq!(sanitize_component("..."), "");
        assert_eq!(sanitize_component("\u{0001}"), "");
    }

    #[test]
    fn renders_all_tokens() {
        let rendered = render_template("{creator} - {title} [{quality}] {platform} {date}", &ctx());
        assert_eq!(rendered, "creator - My Video [1080p] youtube 2026-09-07");
    }

    #[test]
    fn collapses_separators_around_a_missing_token() {
        let mut context = ctx();
        context.creator = None;
        assert_eq!(render_template("{creator} - {title}", &context), "My Video");
    }

    #[test]
    fn keeps_unknown_tokens_visible() {
        assert_eq!(render_template("{title} {bogus}", &ctx()), "My Video {bogus}");
    }

    #[test]
    fn unterminated_token_is_left_alone() {
        assert_eq!(render_template("{title} {oops", &ctx()), "My Video {oops");
    }

    #[test]
    fn falls_back_to_the_title_when_a_template_renders_empty() {
        assert_eq!(render_template("{creator}", &{
            let mut c = ctx();
            c.creator = None;
            c
        }), "My Video");
    }

    #[test]
    fn truncation_respects_character_boundaries() {
        let long = "ü".repeat(400);
        let cut = truncate_stem(&long, 50);
        assert_eq!(cut.chars().count(), 50);
    }

    #[test]
    fn truncation_prefers_a_word_boundary() {
        let text = "one two three four five six seven eight nine ten eleven";
        let cut = truncate_stem(text, 30);
        assert!(!cut.ends_with(' '));
        assert!(cut.len() <= 30);
        assert!(text.starts_with(&cut));
    }

    #[test]
    fn a_title_in_another_script_keeps_as_much_as_a_latin_one() {
        for letter in ['a', 'ж', 'あ'] {
            let early: String = std::iter::repeat(letter).take(30).collect();
            let rest: String = std::iter::repeat(letter).take(119).collect();
            let cut = truncate_stem(&format!("{early} {rest}"), 120);
            assert_eq!(cut.chars().count(), 120, "{letter}");
        }
    }

    #[test]
    fn an_extension_written_in_the_template_is_not_added_twice() {
        assert_eq!(render_template("{title}.{ext}", &ctx()), render_template("{title}", &ctx()));
        assert_eq!(render_template("{title}{ext} ", &ctx()), render_template("{title}", &ctx()));
        // Anywhere else it is the user's own.
        assert!(render_template("{ext} - {title}", &ctx()).starts_with("mp4 - "));
    }

    #[test]
    fn a_slash_in_the_template_makes_folders() {
        let context = ctx();
        let template = "{platform}/{creator}\\{title} [{quality}]";
        assert_eq!(render_folders(template, &context), [context.platform, context.creator.unwrap()]);
        assert_eq!(render_template(template, &context), render_template("{title} [{quality}]", &context));
        assert!(render_folders("{title}", &context).is_empty());
    }

    #[test]
    fn a_folder_cannot_climb_out_or_be_opened_by_a_title() {
        let mut context = ctx();
        context.title = "a/b\\c";
        context.creator = None;
        assert_eq!(render_folders("../..\\{creator}/ /{title}/x", &context), ["a-b-c"]);
        assert_eq!(render_template("../{title}", &context), "a-b-c");
    }

    #[test]
    fn short_names_are_not_truncated() {
        assert_eq!(truncate_stem("short", 100), "short");
    }
}
