//! Channel resolution and the list of long videos.

use std::time::Duration;

use serde::Deserialize;

use crate::model::Video;
use crate::ytdlp::{YtDlp, YtDlpError};

const YOUTUBE: &str = "https://www.youtube.com";

/// A channel, as the user gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChannelRef {
    /// A handle without the "@".
    Handle(String),
    /// A channel ID (`UC…`).
    Id(String),
    /// An old `/c/<name>` or `/user/<name>` path.
    LegacyPath(String),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ChannelRefError {
    #[error("the channel is empty")]
    Empty,
    #[error(
        "\"{0}\" is not a channel. Use the @handle (for example @Skateiq), the channel URL, or \
         the channel ID (UC…)"
    )]
    Invalid(String),
}

impl ChannelRef {
    pub(crate) fn parse(input: &str) -> Result<Self, ChannelRefError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(ChannelRefError::Empty);
        }
        let invalid = || ChannelRefError::Invalid(input.to_owned());

        if let Some(path) = youtube_path(input) {
            let mut segments = path.split('/').filter(|segment| !segment.is_empty());
            let first = segments.next().ok_or_else(invalid)?;
            if let Some(handle) = first.strip_prefix('@') {
                return valid_handle(handle).map(Self::Handle).ok_or_else(invalid);
            }
            let second = segments.next().ok_or_else(invalid)?;
            return match first {
                "channel" if is_channel_id(second) => Ok(Self::Id(second.to_owned())),
                "c" | "user" if valid_name(second) => {
                    Ok(Self::LegacyPath(format!("{first}/{second}")))
                }
                _ => Err(invalid()),
            };
        }
        if let Some(handle) = input.strip_prefix('@') {
            return valid_handle(handle).map(Self::Handle).ok_or_else(invalid);
        }
        if is_channel_id(input) {
            return Ok(Self::Id(input.to_owned()));
        }
        valid_handle(input).map(Self::Handle).ok_or_else(invalid)
    }

    /// The URL of the "Videos" tab. This tab does not contain Shorts or live streams.
    pub(crate) fn videos_tab_url(&self) -> String {
        match self {
            Self::Handle(handle) => format!("{YOUTUBE}/@{handle}/videos"),
            Self::Id(id) => format!("{YOUTUBE}/channel/{id}/videos"),
            Self::LegacyPath(path) => format!("{YOUTUBE}/{path}/videos"),
        }
    }
}

/// The path part of a YouTube URL, without the query. `None` if the input is not a YouTube URL.
fn youtube_path(input: &str) -> Option<&str> {
    let rest = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
        .unwrap_or(input);
    let rest = rest
        .strip_prefix("www.")
        .or_else(|| rest.strip_prefix("m."))
        .unwrap_or(rest);
    let path = rest.strip_prefix("youtube.com/")?;
    let end = path.find(['?', '#']).unwrap_or(path.len());
    Some(&path[..end])
}

fn is_channel_id(text: &str) -> bool {
    text.len() == 24 && text.starts_with("UC") && text.chars().all(is_id_char)
}

fn is_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| is_id_char(c) || c == '.')
}

/// Handles contain letters, digits, "_", "-" and ".". Letters can be non-ASCII.
fn valid_handle(handle: &str) -> Option<String> {
    let ok = !handle.is_empty()
        && handle
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.');
    ok.then(|| handle.to_owned())
}

/// The channel and its long videos.
#[derive(Debug)]
pub(crate) struct ChannelListing {
    pub(crate) name: String,
    pub(crate) channel_id: String,
    /// The handle with the "@", if YouTube gives it.
    pub(crate) handle: Option<String>,
    pub(crate) videos: Vec<Video>,
    pub(crate) removed: RemovedCounts,
}

impl ChannelListing {
    /// The name of the channel folder: the handle without the "@", or the channel ID.
    pub(crate) fn folder_name(&self) -> String {
        self.handle
            .as_deref()
            .map(|handle| handle.trim_start_matches('@'))
            .filter(|name| !name.is_empty())
            .map_or_else(|| self.channel_id.clone(), str::to_owned)
    }
}

/// The number of entries that the filter removed, for each reason.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RemovedCounts {
    pub(crate) shorts: usize,
    pub(crate) live: usize,
    pub(crate) too_short: usize,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ListError {
    #[error("the channel is not found: {0}")]
    NotFound(String),
    #[error(transparent)]
    YtDlp(YtDlpError),
    #[error("cannot read the video list from yt-dlp: {0}")]
    Parse(#[from] serde_json::Error),
}

/// Get the list of long videos of the channel.
pub(crate) fn list_videos(
    ytdlp: &YtDlp,
    channel: &ChannelRef,
    min_duration: Option<Duration>,
) -> Result<ChannelListing, ListError> {
    let url = channel.videos_tab_url();
    tracing::info!("Getting the video list from {url}");
    let args = [
        "--flat-playlist",
        "--dump-single-json",
        "--no-warnings",
        "--",
        url.as_str(),
    ];
    let json = match ytdlp.run(&args) {
        Ok(json) => json,
        Err(YtDlpError::Failed { stderr }) if stderr.contains("does not have a videos tab") => {
            tracing::warn!("The channel has no \"Videos\" tab, thus it has no long videos");
            return empty_listing(channel);
        }
        Err(YtDlpError::Failed { stderr })
            if stderr.contains("does not exist") || stderr.contains("HTTP Error 404") =>
        {
            return Err(ListError::NotFound(stderr));
        }
        Err(error) => return Err(ListError::YtDlp(error)),
    };
    let playlist: FlatPlaylist = serde_json::from_slice(&json)?;
    Ok(playlist.into_listing(min_duration))
}

fn empty_listing(channel: &ChannelRef) -> Result<ChannelListing, ListError> {
    let (name, channel_id, handle) = match channel {
        ChannelRef::Handle(handle) => (handle.clone(), handle.clone(), Some(format!("@{handle}"))),
        ChannelRef::Id(id) => (id.clone(), id.clone(), None),
        ChannelRef::LegacyPath(path) => (path.clone(), path.replace('/', "_"), None),
    };
    Ok(ChannelListing {
        name,
        channel_id,
        handle,
        videos: Vec::new(),
        removed: RemovedCounts::default(),
    })
}

/// The part of the `yt-dlp --flat-playlist --dump-single-json` output that the tool uses.
#[derive(Debug, Deserialize)]
struct FlatPlaylist {
    id: String,
    channel: Option<String>,
    channel_id: Option<String>,
    uploader_id: Option<String>,
    #[serde(default)]
    entries: Vec<FlatEntry>,
}

#[derive(Debug, Deserialize)]
struct FlatEntry {
    id: String,
    title: Option<String>,
    url: Option<String>,
    duration: Option<f64>,
    live_status: Option<LiveStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LiveStatus {
    NotLive,
    IsLive,
    IsUpcoming,
    WasLive,
    PostLive,
    #[serde(other)]
    Unknown,
}

impl FlatPlaylist {
    fn into_listing(self, min_duration: Option<Duration>) -> ChannelListing {
        let channel_id = self.channel_id.unwrap_or(self.id);
        let name = self.channel.unwrap_or_else(|| channel_id.clone());
        let (videos, removed) = filter_entries(self.entries, min_duration);
        ChannelListing {
            name,
            channel_id,
            handle: self.uploader_id,
            videos,
            removed,
        }
    }
}

fn filter_entries(
    entries: Vec<FlatEntry>,
    min_duration: Option<Duration>,
) -> (Vec<Video>, RemovedCounts) {
    let mut removed = RemovedCounts::default();
    let mut videos = Vec::new();
    for entry in entries {
        if entry
            .url
            .as_deref()
            .is_some_and(|url| url.contains("/shorts/"))
        {
            removed.shorts += 1;
            continue;
        }
        let is_live = match entry.live_status {
            None | Some(LiveStatus::NotLive) => false,
            Some(LiveStatus::IsLive | LiveStatus::IsUpcoming | LiveStatus::WasLive)
            | Some(LiveStatus::PostLive) => true,
            Some(LiveStatus::Unknown) => {
                tracing::warn!(
                    "Video {} has an unknown live status. It stays in the list",
                    entry.id
                );
                false
            }
        };
        if is_live {
            removed.live += 1;
            continue;
        }
        let duration = entry
            .duration
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .map(Duration::from_secs_f64);
        if let (Some(min), Some(duration)) = (min_duration, duration)
            && duration < min
        {
            removed.too_short += 1;
            continue;
        }
        let title = entry.title.unwrap_or_else(|| entry.id.clone());
        videos.push(Video {
            id: entry.id,
            title,
            duration,
        });
    }
    (videos, removed)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{ChannelRef, ChannelRefError, FlatEntry, FlatPlaylist, LiveStatus, RemovedCounts};

    fn parse(input: &str) -> Result<ChannelRef, ChannelRefError> {
        ChannelRef::parse(input)
    }

    #[test]
    fn parses_handles() {
        let expected = Ok(ChannelRef::Handle("Skateiq".to_owned()));
        assert_eq!(parse("@Skateiq"), expected);
        assert_eq!(parse("Skateiq"), expected);
        assert_eq!(parse("  @Skateiq "), expected);
        assert_eq!(parse("https://www.youtube.com/@Skateiq"), expected);
        assert_eq!(parse("https://www.youtube.com/@Skateiq/videos"), expected);
        assert_eq!(parse("youtube.com/@Skateiq/shorts?x=1"), expected);
        assert_eq!(parse("http://m.youtube.com/@Skateiq#top"), expected);
    }

    #[test]
    fn parses_channel_ids() {
        let id = "UC-Pk-TTaSrDOjkPKQ7C8HXQ";
        let expected = Ok(ChannelRef::Id(id.to_owned()));
        assert_eq!(parse(id), expected);
        assert_eq!(
            parse(&format!("https://www.youtube.com/channel/{id}/featured")),
            expected
        );
    }

    #[test]
    fn parses_legacy_paths() {
        assert_eq!(
            parse("https://www.youtube.com/c/SkateIQ"),
            Ok(ChannelRef::LegacyPath("c/SkateIQ".to_owned()))
        );
        assert_eq!(
            parse("https://www.youtube.com/user/someone/videos"),
            Ok(ChannelRef::LegacyPath("user/someone".to_owned()))
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(parse("   "), Err(ChannelRefError::Empty));
        for input in [
            "Skate IQ",
            "@",
            "https://www.youtube.com/watch?v=abc",
            "https://www.youtube.com/channel/nope",
        ] {
            assert_eq!(
                parse(input),
                Err(ChannelRefError::Invalid(input.to_owned()))
            );
        }
    }

    #[test]
    fn makes_videos_tab_urls() {
        let handle = ChannelRef::Handle("Skateiq".to_owned());
        assert_eq!(
            handle.videos_tab_url(),
            "https://www.youtube.com/@Skateiq/videos"
        );
        let id = ChannelRef::Id("UC-Pk-TTaSrDOjkPKQ7C8HXQ".to_owned());
        assert_eq!(
            id.videos_tab_url(),
            "https://www.youtube.com/channel/UC-Pk-TTaSrDOjkPKQ7C8HXQ/videos"
        );
    }

    #[test]
    fn reads_the_real_channel_list() {
        let json = include_str!("../tests/fixtures/ytdlp_channel_videos.json");
        let playlist: FlatPlaylist = serde_json::from_str(json).unwrap();
        let listing = playlist.into_listing(None);
        assert_eq!(listing.name, "Skate IQ");
        assert_eq!(listing.channel_id, "UC-Pk-TTaSrDOjkPKQ7C8HXQ");
        assert_eq!(listing.handle.as_deref(), Some("@Skateiq"));
        assert_eq!(listing.folder_name(), "Skateiq");
        assert_eq!(listing.videos.len(), 182);
        assert_eq!(listing.removed, RemovedCounts::default());
        let first = &listing.videos[0];
        assert_eq!(first.id, "WlmQHrgV7xA");
        assert_eq!(first.title, "Josh Dirksen Can Do It All | Break 100");
        assert_eq!(first.duration, Some(Duration::from_secs(2531)));
    }

    fn entry(id: &str, url: &str, duration: Option<f64>, live: Option<LiveStatus>) -> FlatEntry {
        FlatEntry {
            id: id.to_owned(),
            title: Some(format!("Title {id}")),
            url: Some(url.to_owned()),
            duration,
            live_status: live,
        }
    }

    #[test]
    fn filter_removes_shorts_live_and_short_videos() {
        let entries = vec![
            entry(
                "long1",
                "https://www.youtube.com/watch?v=long1",
                Some(600.0),
                None,
            ),
            entry(
                "short1",
                "https://www.youtube.com/shorts/short1",
                Some(30.0),
                None,
            ),
            entry(
                "live1",
                "https://www.youtube.com/watch?v=live1",
                None,
                Some(LiveStatus::IsLive),
            ),
            entry(
                "was1",
                "https://www.youtube.com/watch?v=was1",
                Some(5000.0),
                Some(LiveStatus::WasLive),
            ),
            entry(
                "up1",
                "https://www.youtube.com/watch?v=up1",
                None,
                Some(LiveStatus::IsUpcoming),
            ),
            entry(
                "post1",
                "https://www.youtube.com/watch?v=post1",
                None,
                Some(LiveStatus::PostLive),
            ),
            entry(
                "brief",
                "https://www.youtube.com/watch?v=brief",
                Some(40.0),
                Some(LiveStatus::NotLive),
            ),
            entry("nodur", "https://www.youtube.com/watch?v=nodur", None, None),
        ];
        let (videos, removed) = super::filter_entries(entries, Some(Duration::from_secs(60)));
        let ids: Vec<&str> = videos.iter().map(|video| video.id.as_str()).collect();
        assert_eq!(ids, ["long1", "nodur"]);
        assert_eq!(
            removed,
            RemovedCounts {
                shorts: 1,
                live: 4,
                too_short: 1
            }
        );
    }

    #[test]
    fn unknown_live_status_is_kept() {
        let json = r#"{"id": "x", "entries": [{"id": "a", "live_status": "something_new"}]}"#;
        let playlist: FlatPlaylist = serde_json::from_str(json).unwrap();
        let listing = playlist.into_listing(None);
        assert_eq!(listing.videos.len(), 1);
        assert_eq!(listing.folder_name(), "x");
    }
}
