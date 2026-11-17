//! The download loop, with retries, cool-downs and the circuit breaker (PLAN.md, section 7).

use std::time::Duration;

use crate::error::FetchError;
use crate::http::HttpClient;
use crate::model::{Segment, TrackKind, TrackList, Video};
use crate::store::{Status, Store, StoreError, TranscriptDoc};
use crate::throttle::{
    Interrupted, MAX_COOLDOWNS, MAX_TRANSIENT_RETRIES, MAX_UNEXPECTED_IN_SEQUENCE, Sleeper,
    Throttle,
};
use crate::transcript::{self, Reason, TrackSource};

#[derive(Debug)]
pub(crate) struct Settings {
    pub(crate) fallback_lang: String,
    pub(crate) max_videos: Option<usize>,
}

/// Why the loop stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stop {
    /// All pending videos are processed, or `--max-videos` is reached.
    Finished,
    /// The circuit breaker stopped the run. The current video stays pending.
    Blocked(String),
    /// The user pressed Ctrl-C.
    Interrupted,
    /// Too many unexpected responses in sequence. YouTube probably changed its site.
    TooManyUnexpected,
}

/// A downloaded transcript.
#[derive(Debug)]
struct Fetched {
    language: String,
    kind: TrackKind,
    upload_date: Option<String>,
    segments: Vec<Segment>,
}

/// The result of all attempts for one video.
#[derive(Debug)]
enum Attempt {
    Fetched(Fetched),
    Failed(FetchError),
    Interrupted,
}

pub(crate) struct Runner<'a> {
    source: &'a mut dyn TrackSource,
    http: &'a HttpClient,
    store: &'a mut Store,
    channel: &'a str,
    settings: &'a Settings,
    throttle: &'a mut Throttle,
    sleeper: &'a mut dyn Sleeper,
    /// True after a cool-down, until a request gives a response that is not HTTP 429.
    probing: bool,
    cooldowns: u32,
}

impl<'a> Runner<'a> {
    pub(crate) fn new(
        source: &'a mut dyn TrackSource,
        http: &'a HttpClient,
        store: &'a mut Store,
        channel: &'a str,
        settings: &'a Settings,
        throttle: &'a mut Throttle,
        sleeper: &'a mut dyn Sleeper,
    ) -> Self {
        Self {
            source,
            http,
            store,
            channel,
            settings,
            throttle,
            sleeper,
            probing: false,
            cooldowns: 0,
        }
    }

    /// Process the pending videos, one at a time.
    pub(crate) fn run(&mut self, pending: &[&Video]) -> Result<Stop, StoreError> {
        let limit = self.settings.max_videos.unwrap_or(usize::MAX);
        let total = pending.len().min(limit);
        let mut unexpected_in_sequence = 0;
        for (index, video) in pending.iter().take(limit).enumerate() {
            let wait = self.throttle.before_video(index);
            if wait > Duration::from_secs(60) {
                tracing::info!("Pause: waiting {}", humantime::format_duration(round(wait)));
            }
            if self.sleeper.sleep(wait).is_err() {
                return Ok(Stop::Interrupted);
            }
            tracing::info!("[{}/{total}] {} ({})", index + 1, video.title, video.id);
            let error = match self.fetch_with_retries(video) {
                Attempt::Fetched(fetched) => {
                    self.save(video, &fetched)?;
                    unexpected_in_sequence = 0;
                    continue;
                }
                Attempt::Interrupted => return Ok(Stop::Interrupted),
                Attempt::Failed(error) => error,
            };
            let message = error.to_string();
            let status = match error {
                FetchError::Blocked(reason) => return Ok(Stop::Blocked(reason)),
                FetchError::RateLimited { .. } => {
                    return Ok(Stop::Blocked(
                        "HTTP 429 again after the cool-down".to_owned(),
                    ));
                }
                FetchError::NoTranscript => Status::NoTranscript,
                FetchError::Unavailable(_) => Status::Unavailable,
                FetchError::Transient(_) => Status::Failed,
                FetchError::Unexpected { body, .. } => {
                    if let Some(body) = body {
                        let path = self.store.save_debug(&video.id, &body)?;
                        tracing::warn!("  Saved the response to {}", path.display());
                    }
                    Status::Error
                }
            };
            tracing::warn!("  {message}");
            self.store.record_failure(&video.id, status, &message)?;
            if status == Status::Error {
                unexpected_in_sequence += 1;
                if unexpected_in_sequence >= MAX_UNEXPECTED_IN_SEQUENCE {
                    return Ok(Stop::TooManyUnexpected);
                }
            } else {
                unexpected_in_sequence = 0;
            }
        }
        Ok(Stop::Finished)
    }

    /// Fetch one video. Retry after transient errors. After the first HTTP 429, wait for the
    /// cool-down and send one probe.
    fn fetch_with_retries(&mut self, video: &Video) -> Attempt {
        let mut retries = 0;
        loop {
            let error = match self.fetch(video) {
                Ok(fetched) => {
                    self.probing = false;
                    return Attempt::Fetched(fetched);
                }
                Err(error) => error,
            };
            let wait = match &error {
                FetchError::Transient(message) if retries < MAX_TRANSIENT_RETRIES => {
                    let wait = self.throttle.backoff(retries);
                    retries += 1;
                    tracing::warn!(
                        "  {message}. Retry {retries}/{MAX_TRANSIENT_RETRIES} in {}",
                        humantime::format_duration(round(wait))
                    );
                    wait
                }
                FetchError::RateLimited { retry_after }
                    if !self.probing && self.cooldowns < MAX_COOLDOWNS =>
                {
                    self.probing = true;
                    self.cooldowns += 1;
                    let wait = self.throttle.cooldown(*retry_after);
                    tracing::warn!(
                        "  HTTP 429 Too Many Requests. Waiting {} before one probe request",
                        humantime::format_duration(round(wait))
                    );
                    wait
                }
                FetchError::RateLimited { .. } => return Attempt::Failed(error),
                FetchError::Transient(_)
                | FetchError::NoTranscript
                | FetchError::Unavailable(_)
                | FetchError::Blocked(_)
                | FetchError::Unexpected { .. } => {
                    self.probing = false;
                    return Attempt::Failed(error);
                }
            };
            if let Err(Interrupted) = self.sleeper.sleep(wait) {
                return Attempt::Interrupted;
            }
        }
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
        let pause = self.throttle.step_pause();
        self.sleeper.pause(pause);
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
}

/// Round to whole seconds, for the log.
fn round(duration: Duration) -> Duration {
    Duration::from_secs(duration.as_secs())
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
                "  The original language is not clear, and there is no --lang track. Using the \
                 first track ({language})"
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use mockito::{Matcher, Mock, Server, ServerGuard};
    use reqwest::Url;
    use serde_json::json;

    use super::{Runner, Settings, Stop};
    use crate::http::HttpClient;
    use crate::model::Video;
    use crate::store::{Manifest, Status, Store};
    use crate::throttle::{Interrupted, Sleeper, Throttle, ThrottleSettings};
    use crate::transcript::Innertube;

    const JSON3: &str = include_str!("../tests/fixtures/timedtext_manual.json3");

    /// Records the waits. It can simulate Ctrl-C at a given long wait.
    #[derive(Debug, Default)]
    struct FakeSleeper {
        sleeps: Vec<Duration>,
        pauses: usize,
        interrupt_at: Option<usize>,
    }

    impl Sleeper for FakeSleeper {
        fn sleep(&mut self, duration: Duration) -> Result<(), Interrupted> {
            if self.interrupt_at == Some(self.sleeps.len()) {
                return Err(Interrupted);
            }
            self.sleeps.push(duration);
            Ok(())
        }

        fn pause(&mut self, _duration: Duration) {
            self.pauses += 1;
        }
    }

    fn videos(count: usize) -> Vec<Video> {
        (0..count)
            .map(|index| Video {
                id: format!("video{index}"),
                title: format!("Video {index}"),
                duration: Some(Duration::from_secs(600)),
            })
            .collect()
    }

    fn player_body(server: &ServerGuard) -> String {
        json!({
            "playabilityStatus": {"status": "OK"},
            "streamingData": {"adaptiveFormats": [
                {"audioTrack": {"id": "en-US.4", "audioIsDefault": true}},
                {"audioTrack": {"id": "de-DE.10", "audioIsDefault": false, "isAutoDubbed": true}}
            ]},
            "captions": {"playerCaptionsTracklistRenderer": {"captionTracks": [
                {"baseUrl": format!("{}/api/timedtext?lang=de&kind=asr&fmt=srv3", server.url()),
                 "languageCode": "de", "kind": "asr"},
                {"baseUrl": format!("{}/api/timedtext?lang=en&kind=asr&fmt=srv3", server.url()),
                 "languageCode": "en", "kind": "asr"}
            ]}}
        })
        .to_string()
    }

    /// The watch page. The default expectation is exactly one request in the run.
    fn watch_mock(server: &mut ServerGuard) -> Mock {
        server
            .mock("GET", "/watch")
            .match_query(Matcher::Any)
            .with_body(r#"<script>ytcfg.set({"INNERTUBE_API_KEY":"TESTKEY"})</script>"#)
            .create()
    }

    /// The player request. Call `create` on the result.
    fn player_mock(server: &mut ServerGuard) -> Mock {
        let body = player_body(server);
        server
            .mock("POST", "/youtubei/v1/player")
            .match_query(Matcher::UrlEncoded("key".into(), "TESTKEY".into()))
            .with_body(body)
            .expect_at_least(0)
    }

    /// The English track in json3. Call `create` on the result.
    fn timedtext(server: &mut ServerGuard) -> Mock {
        server
            .mock("GET", "/api/timedtext")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("lang".into(), "en".into()),
                Matcher::UrlEncoded("fmt".into(), "json3".into()),
            ]))
    }

    struct Outcome {
        stop: Stop,
        manifest: Manifest,
        sleeps: Vec<u64>,
        dir: tempfile::TempDir,
    }

    fn run(server: &ServerGuard, count: usize, sleeper: &mut FakeSleeper) -> Outcome {
        let dir = tempfile::tempdir().unwrap();
        let channel_dir = dir.path().join("channel");
        let mut store = Store::open(&channel_dir).unwrap();
        let http = HttpClient::new(None).unwrap();
        let mut source = Innertube::with_base_url(&http, Url::parse(&server.url()).unwrap());
        let settings = Settings {
            fallback_lang: "en".to_owned(),
            max_videos: None,
        };
        let mut throttle = Throttle::with_seed(
            ThrottleSettings {
                delay: Duration::from_secs(10),
                jitter: 0.0,
                batch_size: 50,
                batch_pause: Duration::from_secs(300),
                cooldown: Duration::from_secs(600),
            },
            1,
        );
        let videos = videos(count);
        let pending: Vec<&Video> = videos.iter().collect();
        let stop = Runner::new(
            &mut source,
            &http,
            &mut store,
            "Channel",
            &settings,
            &mut throttle,
            sleeper,
        )
        .run(&pending)
        .unwrap();
        Outcome {
            stop,
            manifest: Manifest::load(&channel_dir).unwrap(),
            sleeps: sleeper.sleeps.iter().map(Duration::as_secs).collect(),
            dir,
        }
    }

    #[test]
    fn downloads_all_videos() {
        let mut server = Server::new();
        let watch = watch_mock(&mut server);
        let player = player_mock(&mut server).expect(2).create();
        let track = timedtext(&mut server).with_body(JSON3).expect(2).create();
        let mut sleeper = FakeSleeper::default();
        let outcome = run(&server, 2, &mut sleeper);

        assert_eq!(outcome.stop, Stop::Finished);
        watch.assert();
        player.assert();
        track.assert();
        assert_eq!(outcome.sleeps, [0, 10]);
        assert_eq!(sleeper.pauses, 2);
        assert_eq!(outcome.manifest.status("video0"), Some(Status::Done));
        assert_eq!(outcome.manifest.status("video1"), Some(Status::Done));
        let file = outcome
            .dir
            .path()
            .join("channel/transcripts/video1_video-1.md");
        let text = fs::read_to_string(file).unwrap();
        assert!(text.contains("language: \"en\"\nauto_generated: true\n"));
        assert!(text.contains("[00:04] And then whenever they hit the tail,\n"));
    }

    #[test]
    fn retries_after_a_transient_error() {
        let mut server = Server::new();
        watch_mock(&mut server);
        player_mock(&mut server).create();
        let error = timedtext(&mut server).with_status(503).expect(1).create();
        let ok = timedtext(&mut server).with_body(JSON3).expect(1).create();
        let outcome = run(&server, 1, &mut FakeSleeper::default());

        assert_eq!(outcome.stop, Stop::Finished);
        error.assert();
        ok.assert();
        assert_eq!(outcome.sleeps, [0, 30]);
        assert_eq!(outcome.manifest.status("video0"), Some(Status::Done));
    }

    #[test]
    fn records_failed_after_all_retries() {
        let mut server = Server::new();
        watch_mock(&mut server);
        player_mock(&mut server).create();
        let error = timedtext(&mut server).with_status(500).expect(4).create();
        let outcome = run(&server, 1, &mut FakeSleeper::default());

        assert_eq!(outcome.stop, Stop::Finished);
        error.assert();
        assert_eq!(outcome.sleeps, [0, 30, 60, 120]);
        assert_eq!(outcome.manifest.status("video0"), Some(Status::Failed));
    }

    #[test]
    fn cooldown_after_429_then_success() {
        let mut server = Server::new();
        watch_mock(&mut server);
        player_mock(&mut server).create();
        let limited = timedtext(&mut server).with_status(429).expect(1).create();
        let ok = timedtext(&mut server).with_body(JSON3).expect(2).create();
        let outcome = run(&server, 2, &mut FakeSleeper::default());

        assert_eq!(outcome.stop, Stop::Finished);
        limited.assert();
        ok.assert();
        assert_eq!(outcome.sleeps, [0, 600, 10]);
        assert_eq!(outcome.manifest.status("video0"), Some(Status::Done));
        assert_eq!(outcome.manifest.status("video1"), Some(Status::Done));
    }

    #[test]
    fn cooldown_obeys_retry_after() {
        let mut server = Server::new();
        watch_mock(&mut server);
        player_mock(&mut server).create();
        timedtext(&mut server)
            .with_status(429)
            .with_header("Retry-After", "120")
            .expect(1)
            .create();
        timedtext(&mut server).with_body(JSON3).expect(1).create();
        let outcome = run(&server, 1, &mut FakeSleeper::default());

        assert_eq!(outcome.stop, Stop::Finished);
        assert_eq!(outcome.sleeps, [0, 120]);
    }

    #[test]
    fn second_429_stops_the_run() {
        let mut server = Server::new();
        watch_mock(&mut server);
        player_mock(&mut server).create();
        let limited = timedtext(&mut server)
            .with_status(429)
            .with_body(include_str!("../tests/fixtures/google_sorry_429.html"))
            .expect(2)
            .create();
        let outcome = run(&server, 3, &mut FakeSleeper::default());

        assert_eq!(
            outcome.stop,
            Stop::Blocked("HTTP 429 again after the cool-down".to_owned())
        );
        limited.assert();
        assert_eq!(outcome.sleeps, [0, 600]);
        assert_eq!(outcome.manifest.status("video0"), None);
        assert_eq!(outcome.manifest.status("video1"), None);
    }

    #[test]
    fn bot_check_stops_immediately() {
        let mut server = Server::new();
        watch_mock(&mut server);
        let player = server
            .mock("POST", "/youtubei/v1/player")
            .match_query(Matcher::Any)
            .with_body(
                r#"{"playabilityStatus": {"status": "LOGIN_REQUIRED",
                    "reason": "Sign in to confirm you’re not a bot"}}"#,
            )
            .expect(1)
            .create();
        let track = timedtext(&mut server).expect(0).create();
        let outcome = run(&server, 3, &mut FakeSleeper::default());

        assert_eq!(
            outcome.stop,
            Stop::Blocked("Sign in to confirm you’re not a bot".to_owned())
        );
        player.assert();
        track.assert();
        assert_eq!(outcome.sleeps, [0]);
        assert_eq!(outcome.manifest.status("video0"), None);
    }

    #[test]
    fn stops_after_five_unexpected_responses() {
        let mut server = Server::new();
        watch_mock(&mut server);
        player_mock(&mut server).create();
        let track = timedtext(&mut server)
            .with_body("<html>changed</html>")
            .expect(5)
            .create();
        let outcome = run(&server, 7, &mut FakeSleeper::default());

        assert_eq!(outcome.stop, Stop::TooManyUnexpected);
        track.assert();
        assert_eq!(outcome.manifest.status("video4"), Some(Status::Error));
        assert_eq!(outcome.manifest.status("video5"), None);
        let debug = outcome.dir.path().join("channel/debug/video0.txt");
        assert_eq!(fs::read_to_string(debug).unwrap(), "<html>changed</html>");
    }

    #[test]
    fn records_videos_without_transcripts() {
        let mut server = Server::new();
        watch_mock(&mut server);
        server
            .mock("POST", "/youtubei/v1/player")
            .match_query(Matcher::Any)
            .with_body(r#"{"playabilityStatus": {"status": "OK"}}"#)
            .expect(1)
            .create();
        let outcome = run(&server, 1, &mut FakeSleeper::default());

        assert_eq!(outcome.stop, Stop::Finished);
        assert_eq!(
            outcome.manifest.status("video0"),
            Some(Status::NoTranscript)
        );
    }

    #[test]
    fn ctrl_c_stops_between_videos() {
        let mut server = Server::new();
        watch_mock(&mut server);
        player_mock(&mut server).create();
        timedtext(&mut server).with_body(JSON3).expect(1).create();
        let mut sleeper = FakeSleeper {
            interrupt_at: Some(1),
            ..FakeSleeper::default()
        };
        let outcome = run(&server, 3, &mut sleeper);

        assert_eq!(outcome.stop, Stop::Interrupted);
        assert_eq!(outcome.manifest.status("video0"), Some(Status::Done));
        assert_eq!(outcome.manifest.status("video1"), None);
    }
}
