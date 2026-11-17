//! Backend 1: the Innertube `player` request (PLAN.md, section 6.2).

use reqwest::Url;
use serde::Deserialize;
use serde_json::json;

use super::TrackSource;
use super::youtube_consts::{
    API_KEY_MARKER, BOT_REASON_MARKER, CLIENT_NAME, CLIENT_VERSION, CONSENT_FORM_MARKER,
    PLAYER_PATH, WATCH_PATH, YOUTUBE_BASE_URL,
};
use crate::error::FetchError;
use crate::http::HttpClient;
use crate::model::{CaptionTrack, TrackKind, TrackList};

pub(crate) struct Innertube<'a> {
    http: &'a HttpClient,
    base_url: Url,
    /// The key from the watch page. The tool gets it one time per run.
    api_key: Option<String>,
}

impl<'a> Innertube<'a> {
    pub(crate) fn new(http: &'a HttpClient) -> Self {
        let base_url = Url::parse(YOUTUBE_BASE_URL).expect("the YouTube base URL is valid");
        Self::with_base_url(http, base_url)
    }

    pub(crate) fn with_base_url(http: &'a HttpClient, base_url: Url) -> Self {
        Self {
            http,
            base_url,
            api_key: None,
        }
    }

    fn api_key(&mut self, video_id: &str) -> Result<String, FetchError> {
        if let Some(key) = &self.api_key {
            return Ok(key.clone());
        }
        let mut url = self.endpoint(WATCH_PATH)?;
        url.query_pairs_mut().append_pair("v", video_id);
        let mut html = self.http.get(url.as_str())?.body;
        if html.contains(CONSENT_FORM_MARKER) {
            let value = consent_value(&html).ok_or_else(|| {
                FetchError::unexpected("cannot read the cookie consent form", Some(&html))
            })?;
            tracing::debug!("Accepting the YouTube cookie consent");
            self.http
                .add_cookie(&format!("CONSENT=YES+{value}"), &self.base_url);
            html = self.http.get(url.as_str())?.body;
            if html.contains(CONSENT_FORM_MARKER) {
                return Err(FetchError::unexpected(
                    "cannot accept the cookie consent",
                    None,
                ));
            }
        }
        let key = extract_api_key(&html).ok_or_else(|| {
            FetchError::unexpected("the watch page has no Innertube API key", Some(&html))
        })?;
        tracing::debug!("Got the Innertube API key from the watch page");
        self.api_key = Some(key.clone());
        Ok(key)
    }

    fn endpoint(&self, path: &str) -> Result<Url, FetchError> {
        self.base_url
            .join(path)
            .map_err(|error| FetchError::unexpected(format!("bad URL {path}: {error}"), None))
    }
}

impl TrackSource for Innertube<'_> {
    fn list_tracks(&mut self, video_id: &str) -> Result<TrackList, FetchError> {
        let key = self.api_key(video_id)?;
        let mut url = self.endpoint(PLAYER_PATH)?;
        url.query_pairs_mut().append_pair("key", &key);
        let body = json!({
            "context": { "client": { "clientName": CLIENT_NAME, "clientVersion": CLIENT_VERSION } },
            "videoId": video_id,
        });
        let response = self.http.post_json(url.as_str(), &body)?;
        parse_player(&response.body)
    }
}

/// Find `"INNERTUBE_API_KEY": "<key>"` in the watch page.
fn extract_api_key(html: &str) -> Option<String> {
    let start = html.find(API_KEY_MARKER)? + API_KEY_MARKER.len();
    let rest = html[start..].trim_start().strip_prefix(':')?;
    let rest = rest.trim_start().strip_prefix('"')?;
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    let key = &rest[..end];
    (!key.is_empty() && rest[end..].starts_with('"')).then(|| key.to_owned())
}

/// The value of the hidden `v` field of the consent form.
fn consent_value(html: &str) -> Option<&str> {
    const FIELD: &str = "name=\"v\" value=\"";
    let start = html.find(FIELD)? + FIELD.len();
    let end = html[start..].find('"')?;
    Some(&html[start..start + end])
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayerResponse {
    playability_status: Option<PlayabilityStatus>,
    captions: Option<Captions>,
    streaming_data: Option<StreamingData>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayabilityStatus {
    status: Option<String>,
    reason: Option<String>,
    error_screen: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Captions {
    player_captions_tracklist_renderer: Option<TracklistRenderer>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TracklistRenderer {
    #[serde(default)]
    caption_tracks: Vec<RawTrack>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTrack {
    base_url: String,
    language_code: String,
    kind: Option<String>,
    name: Option<TrackName>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrackName {
    simple_text: Option<String>,
    #[serde(default)]
    runs: Vec<Run>,
}

#[derive(Debug, Deserialize)]
struct Run {
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamingData {
    #[serde(default)]
    adaptive_formats: Vec<Format>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Format {
    audio_track: Option<AudioTrack>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AudioTrack {
    id: Option<String>,
    #[serde(default)]
    audio_is_default: bool,
}

/// Read the caption tracks and the original audio language from a player response.
fn parse_player(body: &str) -> Result<TrackList, FetchError> {
    let player: PlayerResponse = serde_json::from_str(body).map_err(|error| {
        FetchError::unexpected(
            format!("the player response is not valid: {error}"),
            Some(body),
        )
    })?;
    if let Some(playability) = &player.playability_status {
        check_playability(playability)?;
    }
    let raw_tracks = player
        .captions
        .and_then(|captions| captions.player_captions_tracklist_renderer)
        .map(|renderer| renderer.caption_tracks)
        .unwrap_or_default();
    if raw_tracks.is_empty() {
        return Err(FetchError::NoTranscript);
    }
    let tracks = raw_tracks.into_iter().map(into_track).collect();
    let original_language = player.streaming_data.and_then(|data| {
        data.adaptive_formats
            .into_iter()
            .filter_map(|format| format.audio_track)
            .find(|track| track.audio_is_default)
            .and_then(|track| track.id)
            .map(|id| id.split('.').next().unwrap_or(&id).to_owned())
    });
    Ok(TrackList {
        tracks,
        original_language,
        upload_date: None,
    })
}

fn check_playability(playability: &PlayabilityStatus) -> Result<(), FetchError> {
    let status = playability.status.as_deref().unwrap_or("OK");
    if status == "OK" {
        return Ok(());
    }
    let reason = playability.reason.as_deref().unwrap_or("");
    if status == "LOGIN_REQUIRED" && reason.contains(BOT_REASON_MARKER) {
        return Err(FetchError::Blocked(reason.to_owned()));
    }
    let subreason = playability
        .error_screen
        .as_ref()
        .and_then(|screen| screen.pointer("/playerErrorMessageRenderer/subreason/runs"))
        .and_then(serde_json::Value::as_array)
        .map(|runs| {
            runs.iter()
                .filter_map(|run| run.get("text").and_then(serde_json::Value::as_str))
                .collect::<String>()
        })
        .unwrap_or_default();
    let details = [status, reason, subreason.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(": ");
    Err(FetchError::Unavailable(details))
}

fn into_track(raw: RawTrack) -> CaptionTrack {
    let kind = match raw.kind.as_deref() {
        Some("asr") => TrackKind::AutoGenerated,
        Some(_) | None => TrackKind::Manual,
    };
    let name = raw
        .name
        .map(|name| {
            name.simple_text
                .unwrap_or_else(|| name.runs.into_iter().map(|run| run.text).collect())
        })
        .unwrap_or_else(|| raw.language_code.clone());
    CaptionTrack {
        language_code: raw.language_code,
        kind,
        name,
        url: raw.base_url,
    }
}

#[cfg(test)]
mod tests {
    use super::{consent_value, extract_api_key, parse_player};
    use crate::error::FetchError;
    use crate::model::TrackKind;

    #[test]
    fn extracts_the_api_key() {
        let html = r#"<script>ytcfg.set({"INNERTUBE_API_KEY": "AIzaTest_Key-1","X":1})</script>"#;
        assert_eq!(extract_api_key(html).as_deref(), Some("AIzaTest_Key-1"));
        assert_eq!(extract_api_key(r#""INNERTUBE_API_KEY":"""#), None);
        assert_eq!(extract_api_key("<html></html>"), None);
    }

    #[test]
    fn reads_the_consent_value() {
        let html = r#"<form action="https://consent.youtube.com/s"><input name="v" value="cb.2026"></form>"#;
        assert_eq!(consent_value(html), Some("cb.2026"));
    }

    #[test]
    fn parses_the_dubbed_player_fixture() {
        let list = parse_player(include_str!("../../tests/fixtures/player_dubbed.json")).unwrap();
        assert_eq!(list.tracks.len(), 21);
        assert_eq!(list.original_language.as_deref(), Some("en-US"));
        assert!(
            list.tracks
                .iter()
                .all(|track| track.kind == TrackKind::AutoGenerated)
        );
        let english = &list.tracks[3];
        assert_eq!(english.language_code, "en");
        assert_eq!(english.name, "English (auto-generated)");
        assert!(
            english
                .url
                .starts_with("https://www.youtube.com/api/timedtext?")
        );
    }

    #[test]
    fn player_without_captions_has_no_transcript() {
        let body = r#"{"playabilityStatus": {"status": "OK"}, "videoDetails": {}}"#;
        assert_eq!(parse_player(body).unwrap_err(), FetchError::NoTranscript);
    }

    #[test]
    fn bot_check_is_a_block() {
        let body = r#"{"playabilityStatus": {"status": "LOGIN_REQUIRED",
                       "reason": "Sign in to confirm you’re not a bot"}}"#;
        assert_eq!(
            parse_player(body).unwrap_err(),
            FetchError::Blocked("Sign in to confirm you’re not a bot".to_owned())
        );
    }

    #[test]
    fn unplayable_videos_are_unavailable() {
        let body = r#"{"playabilityStatus": {"status": "ERROR", "reason": "This video is unavailable",
            "errorScreen": {"playerErrorMessageRenderer": {"subreason": {"runs": [
                {"text": "The uploader has "}, {"text": "removed it"}]}}}}}"#;
        assert_eq!(
            parse_player(body).unwrap_err(),
            FetchError::Unavailable(
                "ERROR: This video is unavailable: The uploader has removed it".to_owned()
            )
        );
        let age = r#"{"playabilityStatus": {"status": "LOGIN_REQUIRED",
                      "reason": "Sign in to confirm your age"}}"#;
        assert_eq!(
            parse_player(age).unwrap_err(),
            FetchError::Unavailable("LOGIN_REQUIRED: Sign in to confirm your age".to_owned())
        );
    }

    #[test]
    fn bad_json_is_unexpected() {
        let FetchError::Unexpected { message, .. } = parse_player("<html>").unwrap_err() else {
            panic!("expected an unexpected-response error");
        };
        assert!(message.starts_with("the player response is not valid"));
    }
}
