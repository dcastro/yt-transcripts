//! Storage: the manifest, the Markdown transcripts and the atomic writes (PLAN.md, section 8).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::model::{Segment, TrackKind, Video};

const MANIFEST_FILE: &str = "manifest.json";
const VIDEOS_FILE: &str = "videos.json";
const TRANSCRIPTS_DIR: &str = "transcripts";
const DEBUG_DIR: &str = "debug";
const MAX_SLUG_LEN: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Done,
    NoTranscript,
    Unavailable,
    Failed,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub(crate) status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) language: Option<String>,
    pub(crate) attempts: u32,
    #[serde(default)]
    pub(crate) last_error: Option<String>,
    pub(crate) updated_at: String,
}

/// The state of each video. A video that is not in the manifest is pending.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Manifest {
    entries: BTreeMap<String, Entry>,
}

impl Manifest {
    /// Load the manifest of a channel folder. A missing manifest is empty.
    pub(crate) fn load(channel_dir: &Path) -> Result<Self, StoreError> {
        let path = channel_dir.join(MANIFEST_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(StoreError::io("read", &path, source)),
        };
        serde_json::from_str(&text).map_err(|source| StoreError::Json { path, source })
    }

    pub(crate) fn status(&self, video_id: &str) -> Option<Status> {
        self.entries.get(video_id).map(|entry| entry.status)
    }

    /// The resume rules (PLAN.md, section 8).
    pub(crate) fn should_fetch(&self, video_id: &str, retry_failed: bool) -> bool {
        match self.status(video_id) {
            None => true,
            Some(Status::Done | Status::NoTranscript | Status::Unavailable) => false,
            Some(Status::Failed | Status::Error) => retry_failed,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    #[error("cannot {action} {}: {source}", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[error("cannot read {}: {source}", path.display())]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
}

impl StoreError {
    fn io(action: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.to_owned(),
            source,
        }
    }
}

/// A transcript, ready to write.
#[derive(Debug)]
pub(crate) struct TranscriptDoc<'a> {
    pub(crate) video: &'a Video,
    pub(crate) channel: &'a str,
    pub(crate) language: &'a str,
    pub(crate) kind: TrackKind,
    /// `YYYYMMDD`, if the backend gives it.
    pub(crate) upload_date: Option<&'a str>,
    pub(crate) segments: &'a [Segment],
}

/// The output folder of one channel.
#[derive(Debug)]
pub(crate) struct Store {
    dir: PathBuf,
    manifest: Manifest,
}

impl Store {
    /// Open the channel folder. Make the folders if necessary, and load the manifest.
    pub(crate) fn open(channel_dir: &Path) -> Result<Self, StoreError> {
        let transcripts = channel_dir.join(TRANSCRIPTS_DIR);
        fs::create_dir_all(&transcripts)
            .map_err(|source| StoreError::io("make the folder", &transcripts, source))?;
        let manifest = Manifest::load(channel_dir)?;
        Ok(Self {
            dir: channel_dir.to_owned(),
            manifest,
        })
    }

    pub(crate) fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Write the filtered video list to `videos.json`.
    pub(crate) fn save_video_list(&self, videos: &[Video]) -> Result<(), StoreError> {
        #[derive(Serialize)]
        struct Item<'a> {
            id: &'a str,
            title: &'a str,
            duration_seconds: Option<u64>,
        }
        let items: Vec<Item<'_>> = videos
            .iter()
            .map(|video| Item {
                id: &video.id,
                title: &video.title,
                duration_seconds: video.duration.map(|duration| duration.as_secs()),
            })
            .collect();
        let json = serde_json::to_string_pretty(&items).expect("the video list is serializable");
        write_atomic(&self.dir.join(VIDEOS_FILE), json.as_bytes())
    }

    /// Write the transcript file, then mark the video as done in the manifest.
    pub(crate) fn save_transcript(
        &mut self,
        doc: &TranscriptDoc<'_>,
    ) -> Result<PathBuf, StoreError> {
        let relative = format!(
            "{TRANSCRIPTS_DIR}/{}_{}.md",
            doc.video.id,
            slug(&doc.video.title)
        );
        let path = self.dir.join(&relative);
        let fetched_at = now();
        write_atomic(&path, render_markdown(doc, &fetched_at).as_bytes())?;
        self.update(
            &doc.video.id,
            Status::Done,
            Some(relative),
            Some(doc.language.to_owned()),
            None,
        )?;
        Ok(path)
    }

    /// Record a video that the tool could not download.
    pub(crate) fn record_failure(
        &mut self,
        video_id: &str,
        status: Status,
        error: &str,
    ) -> Result<(), StoreError> {
        self.update(video_id, status, None, None, Some(error.to_owned()))
    }

    /// Save an unexpected response, for debugging.
    pub(crate) fn save_debug(&self, video_id: &str, body: &str) -> Result<PathBuf, StoreError> {
        let dir = self.dir.join(DEBUG_DIR);
        fs::create_dir_all(&dir)
            .map_err(|source| StoreError::io("make the folder", &dir, source))?;
        let path = dir.join(format!("{video_id}.txt"));
        write_atomic(&path, body.as_bytes())?;
        Ok(path)
    }

    fn update(
        &mut self,
        video_id: &str,
        status: Status,
        file: Option<String>,
        language: Option<String>,
        last_error: Option<String>,
    ) -> Result<(), StoreError> {
        let attempts = self
            .manifest
            .entries
            .get(video_id)
            .map_or(0, |entry| entry.attempts)
            + 1;
        let entry = Entry {
            status,
            file,
            language,
            attempts,
            last_error,
            updated_at: now(),
        };
        self.manifest.entries.insert(video_id.to_owned(), entry);
        let json =
            serde_json::to_string_pretty(&self.manifest).expect("the manifest is serializable");
        write_atomic(&self.dir.join(MANIFEST_FILE), json.as_bytes())
    }
}

/// Write to a temporary file in the same folder, then rename it. A crash cannot leave a
/// partial file.
fn write_atomic(path: &Path, content: &[u8]) -> Result<(), StoreError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(dir)
        .map_err(|source| StoreError::io("make a temporary file in", dir, source))?;
    file.write_all(content)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|source| StoreError::io("write", file.path(), source))?;
    file.persist(path)
        .map_err(|error| StoreError::io("write", path, error.error))?;
    Ok(())
}

fn now() -> String {
    humantime::format_rfc3339_seconds(SystemTime::now()).to_string()
}

/// A file name part from a title: lower case ASCII letters and digits, with "-" between words.
fn slug(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            if slug.len() >= MAX_SLUG_LEN {
                break;
            }
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "video".to_owned()
    } else {
        slug.to_owned()
    }
}

/// The Markdown file: YAML front matter, then one line for each segment.
fn render_markdown(doc: &TranscriptDoc<'_>, fetched_at: &str) -> String {
    let mut out = String::from("---\n");
    let video = doc.video;
    // A JSON string is also a valid YAML string, with all special characters escaped.
    let quoted = |text: &str| serde_json::to_string(text).expect("a string is serializable");
    let _ = writeln!(out, "video_id: {}", video.id);
    let _ = writeln!(out, "title: {}", quoted(&video.title));
    let _ = writeln!(out, "url: {}", video.url());
    let _ = writeln!(out, "channel: {}", quoted(doc.channel));
    if let Some(date) = doc.upload_date {
        let _ = writeln!(out, "upload_date: {}", format_date(date));
    }
    if let Some(duration) = video.duration {
        let _ = writeln!(out, "duration_seconds: {}", duration.as_secs());
    }
    let _ = writeln!(out, "language: {}", quoted(doc.language));
    let auto_generated = match doc.kind {
        TrackKind::AutoGenerated => true,
        TrackKind::Manual => false,
    };
    let _ = writeln!(out, "auto_generated: {auto_generated}");
    let _ = writeln!(out, "fetched_at: {fetched_at}");
    out.push_str("---\n\n");

    let last_start = doc
        .segments
        .last()
        .map_or(Duration::ZERO, |segment| segment.start);
    let longest = video.duration.unwrap_or(Duration::ZERO).max(last_start);
    let with_hours = longest >= Duration::from_secs(3600);
    for segment in doc.segments {
        let _ = writeln!(
            out,
            "[{}] {}",
            timestamp(segment.start, with_hours),
            segment.text
        );
    }
    out
}

fn timestamp(start: Duration, with_hours: bool) -> String {
    let seconds = start.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if with_hours {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// `20261106` gives `2026-11-06`. Other values stay as they are, in quotes.
fn format_date(date: &str) -> String {
    if date.len() == 8 && date.chars().all(|c| c.is_ascii_digit()) {
        format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..])
    } else {
        serde_json::to_string(date).expect("a string is serializable")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use super::{Manifest, Status, Store, TranscriptDoc, render_markdown, slug, timestamp};
    use crate::model::{Segment, TrackKind, Video};

    fn video(duration_secs: u64) -> Video {
        Video {
            id: "biHf4BXm4G0".to_owned(),
            title: "How to \"Manual\" Like a Pro: Part #1".to_owned(),
            duration: Some(Duration::from_secs(duration_secs)),
        }
    }

    fn segments() -> Vec<Segment> {
        vec![
            Segment {
                start: Duration::from_millis(160),
                text: "Everyone wants to practice manuals".to_owned(),
            },
            Segment {
                start: Duration::from_millis(4_480),
                text: "And then whenever they hit the tail".to_owned(),
            },
        ]
    }

    #[test]
    fn makes_slugs() {
        assert_eq!(slug("How to Manual Like a Pro"), "how-to-manual-like-a-pro");
        assert_eq!(slug("  Ollie!! -- 101  "), "ollie-101");
        assert_eq!(slug("日本語"), "video");
        assert_eq!(slug(&"a".repeat(80)).len(), 60);
    }

    #[test]
    fn formats_timestamps() {
        assert_eq!(timestamp(Duration::from_millis(3_999), false), "00:03");
        assert_eq!(timestamp(Duration::from_secs(3725), true), "01:02:05");
        assert_eq!(timestamp(Duration::from_secs(65), true), "00:01:05");
    }

    #[test]
    fn renders_markdown() {
        let video = video(1984);
        let segments = segments();
        let doc = TranscriptDoc {
            video: &video,
            channel: "Skate IQ",
            language: "en",
            kind: TrackKind::AutoGenerated,
            upload_date: Some("20250314"),
            segments: &segments,
        };
        let expected = "---\n\
            video_id: biHf4BXm4G0\n\
            title: \"How to \\\"Manual\\\" Like a Pro: Part #1\"\n\
            url: https://www.youtube.com/watch?v=biHf4BXm4G0\n\
            channel: \"Skate IQ\"\n\
            upload_date: 2025-03-14\n\
            duration_seconds: 1984\n\
            language: \"en\"\n\
            auto_generated: true\n\
            fetched_at: 2026-11-17T16:02:11Z\n\
            ---\n\
            \n\
            [00:00] Everyone wants to practice manuals\n\
            [00:04] And then whenever they hit the tail\n";
        assert_eq!(render_markdown(&doc, "2026-11-17T16:02:11Z"), expected);
    }

    #[test]
    fn long_videos_get_hours() {
        let video = video(3600);
        let segments = segments();
        let doc = TranscriptDoc {
            video: &video,
            channel: "c",
            language: "en",
            kind: TrackKind::Manual,
            upload_date: None,
            segments: &segments,
        };
        let text = render_markdown(&doc, "t");
        assert!(text.contains("\n[00:00:00] Everyone"));
        assert!(text.contains("auto_generated: false\n"));
        assert!(!text.contains("upload_date"));
    }

    #[test]
    fn store_writes_files_and_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let channel_dir = dir.path().join("Skateiq");
        let mut store = Store::open(&channel_dir).unwrap();
        let video = video(100);
        store.save_video_list(std::slice::from_ref(&video)).unwrap();
        let segments = segments();
        let doc = TranscriptDoc {
            video: &video,
            channel: "Skate IQ",
            language: "en",
            kind: TrackKind::AutoGenerated,
            upload_date: None,
            segments: &segments,
        };
        let path = store.save_transcript(&doc).unwrap();
        assert_eq!(
            path,
            channel_dir.join("transcripts/biHf4BXm4G0_how-to-manual-like-a-pro-part-1.md")
        );
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("[00:04] And then")
        );
        store
            .record_failure("other", Status::Failed, "timeout")
            .unwrap();
        store
            .record_failure("other", Status::Error, "bad json")
            .unwrap();
        let debug = store.save_debug("other", "<html>").unwrap();
        assert_eq!(fs::read_to_string(debug).unwrap(), "<html>");

        let manifest = Manifest::load(&channel_dir).unwrap();
        assert_eq!(manifest.status("biHf4BXm4G0"), Some(Status::Done));
        assert_eq!(manifest.entries["other"].attempts, 2);
        assert_eq!(
            manifest.entries["other"].last_error.as_deref(),
            Some("bad json")
        );
        assert!(!manifest.should_fetch("biHf4BXm4G0", true));
        assert!(!manifest.should_fetch("other", false));
        assert!(manifest.should_fetch("other", true));
        assert!(manifest.should_fetch("new", false));

        let videos: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(channel_dir.join("videos.json")).unwrap())
                .unwrap();
        assert_eq!(videos[0]["duration_seconds"], 100);
        let leftovers: Vec<_> = fs::read_dir(&channel_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with(".tmp"))
            .collect();
        assert_eq!(leftovers, Vec::<String>::new());
    }

    #[test]
    fn manifest_resume_rules() {
        let json = r#"{
            "a": {"status": "done", "attempts": 1, "updated_at": "t"},
            "b": {"status": "no_transcript", "attempts": 1, "updated_at": "t"},
            "c": {"status": "unavailable", "attempts": 1, "updated_at": "t"},
            "d": {"status": "failed", "attempts": 3, "updated_at": "t"},
            "e": {"status": "error", "attempts": 1, "updated_at": "t"}
        }"#;
        let manifest: Manifest = serde_json::from_str(json).unwrap();
        let fetch = |retry| {
            ["a", "b", "c", "d", "e", "f"]
                .into_iter()
                .filter(|id| manifest.should_fetch(id, retry))
                .collect::<Vec<_>>()
        };
        assert_eq!(fetch(false), ["f"]);
        assert_eq!(fetch(true), ["d", "e", "f"]);
    }
}
