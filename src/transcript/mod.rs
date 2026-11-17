//! Transcript fetch: the track sources, the track selection and the track download.

mod innertube;
mod json3;
mod select;
mod youtube_consts;
mod ytdlp;

use reqwest::Url;

pub(crate) use self::innertube::Innertube;
pub(crate) use self::select::{Reason, select_track};
use self::youtube_consts::PO_TOKEN_MARKER;
pub(crate) use self::ytdlp::YtDlpSource;
use crate::error::FetchError;
use crate::http::HttpClient;
use crate::model::{CaptionTrack, Segment, TrackList};

/// A backend that gets the caption tracks of a video.
pub(crate) trait TrackSource {
    fn list_tracks(&mut self, video_id: &str) -> Result<TrackList, FetchError>;
}

/// Download one caption track in the `json3` format, and parse it.
pub(crate) fn download_track(
    http: &HttpClient,
    track: &CaptionTrack,
) -> Result<Vec<Segment>, FetchError> {
    if track.url.contains(PO_TOKEN_MARKER) {
        return Err(FetchError::unexpected(
            "YouTube requires a PO token for this track",
            None,
        ));
    }
    let url = json3_url(&track.url)?;
    let body = http.get(url.as_str())?.body;
    let segments = json3::parse(&body)?;
    if segments.is_empty() {
        return Err(FetchError::NoTranscript);
    }
    Ok(segments)
}

/// The track URL with `fmt=json3` in place of any other `fmt` value.
fn json3_url(track_url: &str) -> Result<Url, FetchError> {
    let mut url = Url::parse(track_url).map_err(|error| {
        FetchError::unexpected(format!("the track URL is not valid: {error}"), None)
    })?;
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(name, _)| name != "fmt")
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(pairs)
        .append_pair("fmt", "json3");
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::json3_url;

    #[test]
    fn json3_url_replaces_the_format() {
        let url =
            json3_url("https://www.youtube.com/api/timedtext?v=abc&fmt=srv3&lang=en").unwrap();
        assert_eq!(
            url.as_str(),
            "https://www.youtube.com/api/timedtext?v=abc&lang=en&fmt=json3"
        );
        let url = json3_url("https://www.youtube.com/api/timedtext?v=abc").unwrap();
        assert_eq!(
            url.as_str(),
            "https://www.youtube.com/api/timedtext?v=abc&fmt=json3"
        );
    }
}
