//! The download loop.

use std::thread;
use std::time::Duration;

use crate::error::FetchError;
use crate::http::HttpClient;
use crate::model::{Segment, TrackKind, TrackList, Video};
use crate::store::{Status, Store, StoreError, TranscriptDoc};
use crate::transcript::{self, Reason, TrackSource};

/// The pause between the requests for one video (player, then track).
const STEP_PAUSE: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(crate) struct Settings {
    pub(crate) fallback_lang: String,
    pub(crate) delay: Duration,
    pub(crate) max_videos: Option<usize>,
}

/// Why the loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    /// All pending videos are processed, or `--max-videos` is reached.
    Finished,
    /// YouTube blocked the requests.
    Blocked,
}

/// A downloaded transcript.
#[derive(Debug)]
struct Fetched {
    language: String,
    kind: TrackKind,
    upload_date: Option<String>,
    segments: Vec<Segment>,
}

pub(crate) struct Runner<'a> {
    pub(crate) source: &'a mut dyn TrackSource,
    pub(crate) http: &'a HttpClient,
    pub(crate) store: &'a mut Store,
    pub(crate) channel: &'a str,
    pub(crate) settings: &'a Settings,
}

impl Runner<'_> {
    /// Process the pending videos, one at a time.
    pub(crate) fn run(&mut self, pending: &[&Video]) -> Result<Stop, StoreError> {
        let limit = self.settings.max_videos.unwrap_or(usize::MAX);
        let total = pending.len().min(limit);
        for (index, video) in pending.iter().take(limit).enumerate() {
            if index > 0 {
                thread::sleep(self.settings.delay);
            }
            tracing::info!("[{}/{total}] {} ({})", index + 1, video.title, video.id);
            match self.fetch(video) {
                Ok(fetched) => self.save(video, &fetched)?,
                Err(error) => {
                    if self.record(video, &error)? == Stop::Blocked {
                        return Ok(Stop::Blocked);
                    }
                }
            }
        }
        Ok(Stop::Finished)
    }

    fn fetch(&mut self, video: &Video) -> Result<Fetched, FetchError> {
        let list = self.source.list_tracks(&video.id)?;
        let selection = transcript::select_track(&list, &self.settings.fallback_lang)
            .ok_or(FetchError::NoTranscript)?;
        log_selection(
            &list,
            selection.track.language_code.as_str(),
            selection.reason,
        );
        thread::sleep(STEP_PAUSE);
        let segments = transcript::download_track(self.http, selection.track)?;
        Ok(Fetched {
            language: selection.track.language_code.clone(),
            kind: selection.track.kind,
            upload_date: list.upload_date.clone(),
            segments,
        })
    }

    fn save(&mut self, video: &Video, fetched: &Fetched) -> Result<(), StoreError> {
        let doc = TranscriptDoc {
            video,
            channel: self.channel,
            language: &fetched.language,
            kind: fetched.kind,
            upload_date: fetched.upload_date.as_deref(),
            segments: &fetched.segments,
        };
        let path = self.store.save_transcript(&doc)?;
        tracing::info!(
            "  Saved {} lines ({}, {}) to {}",
            fetched.segments.len(),
            fetched.language,
            kind_name(fetched.kind),
            path.display()
        );
        Ok(())
    }

    /// Record a failed video. Return `Stop::Blocked` if the loop must stop.
    fn record(&mut self, video: &Video, error: &FetchError) -> Result<Stop, StoreError> {
        let message = error.to_string();
        let status = match error {
            FetchError::NoTranscript => Status::NoTranscript,
            FetchError::Unavailable(_) => Status::Unavailable,
            FetchError::Transient(_) => Status::Failed,
            FetchError::Unexpected { body, .. } => {
                if let Some(body) = body {
                    let path = self.store.save_debug(&video.id, body)?;
                    tracing::warn!("  Saved the response to {}", path.display());
                }
                Status::Error
            }
            FetchError::RateLimited { .. } | FetchError::Blocked(_) => {
                tracing::error!("  {message}. Stopping");
                return Ok(Stop::Blocked);
            }
        };
        tracing::warn!("  {message}");
        self.store.record_failure(&video.id, status, &message)?;
        Ok(Stop::Finished)
    }
}

fn log_selection(list: &TrackList, language: &str, reason: Reason) {
    let original = list.original_language.as_deref().unwrap_or("unknown");
    match reason {
        Reason::Original => {
            tracing::debug!("  Track {language} is in the original language ({original})");
        }
        Reason::OnlyManualTrack => {
            tracing::info!(
                "  The original language is not clear. Using the only manual track ({language})"
            );
        }
        Reason::FallbackLanguage => {
            tracing::warn!(
                "  The original language is not clear. Using the --lang track ({language})"
            );
        }
        Reason::FirstTrack => {
            tracing::warn!(
                "  The original language is not clear, and there is no --lang track. Using the first track ({language})"
            );
        }
    }
}

fn kind_name(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Manual => "manual",
        TrackKind::AutoGenerated => "auto-generated",
    }
}
