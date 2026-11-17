//! yt-transcripts: download the transcripts of all long videos of a YouTube channel.

mod channel;
mod cli;
mod error;
mod http;
mod model;
mod run;
mod store;
mod transcript;
mod ytdlp;

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;

use crate::channel::{ChannelListing, ChannelRef, ListError};
use crate::cli::Cli;
use crate::http::HttpClient;
use crate::model::Video;
use crate::run::{Runner, Settings, Stop};
use crate::store::{Manifest, Status, Store};
use crate::transcript::Innertube;
use crate::ytdlp::YtDlp;

/// The result of a run, and its exit code (PLAN.md, section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Complete,
    Fatal,
    Usage,
    Blocked,
    SomeFailed,
}

impl Outcome {
    fn exit_code(self) -> ExitCode {
        match self {
            Self::Complete => ExitCode::SUCCESS,
            Self::Fatal => ExitCode::from(1),
            Self::Usage => ExitCode::from(2),
            Self::Blocked => ExitCode::from(3),
            Self::SomeFailed => ExitCode::from(4),
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    run(&cli).exit_code()
}

fn init_logging(verbose: bool) {
    let default = if verbose {
        "yt_transcripts=debug"
    } else {
        "yt_transcripts=info"
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

/// Log an error and give the outcome.
fn fail(error: impl std::fmt::Display, outcome: Outcome) -> Outcome {
    tracing::error!("{error}");
    outcome
}

fn run(cli: &Cli) -> Outcome {
    let channel = match ChannelRef::parse(&cli.channel) {
        Ok(channel) => channel,
        Err(error) => return fail(error, Outcome::Usage),
    };
    let ytdlp = match YtDlp::locate() {
        Ok(ytdlp) => ytdlp,
        Err(error) => return fail(error, Outcome::Fatal),
    };
    tracing::debug!("yt-dlp version {}", ytdlp.version());

    let listing = match channel::list_videos(&ytdlp, &channel, cli.min_duration) {
        Ok(listing) => listing,
        Err(error @ ListError::NotFound(_)) => return fail(error, Outcome::Usage),
        Err(error @ (ListError::YtDlp(_) | ListError::Parse(_))) => {
            return fail(error, Outcome::Fatal);
        }
    };
    let channel_dir = cli.out.join(listing.folder_name());

    if cli.dry_run {
        let manifest = match Manifest::load(&channel_dir) {
            Ok(manifest) => manifest,
            Err(error) => return fail(error, Outcome::Fatal),
        };
        let printed = print_dry_run(&listing, &manifest, &channel_dir, cli.retry_failed);
        return match printed {
            Ok(()) => Outcome::Complete,
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Outcome::Complete,
            Err(error) => fail(
                format!("cannot write to the standard output: {error}"),
                Outcome::Fatal,
            ),
        };
    }

    let mut store = match Store::open(&channel_dir) {
        Ok(store) => store,
        Err(error) => return fail(error, Outcome::Fatal),
    };
    if let Err(error) = store.save_video_list(&listing.videos) {
        return fail(error, Outcome::Fatal);
    }
    let pending = pending_videos(&listing.videos, store.manifest(), cli.retry_failed);
    tracing::info!(
        "{}: {} long videos, {} pending. Output folder: {}",
        listing.name,
        listing.videos.len(),
        pending.len(),
        channel_dir.display()
    );

    let http = match HttpClient::new(None) {
        Ok(http) => http,
        Err(error) => return fail(error, Outcome::Fatal),
    };
    let mut source = Innertube::new(&http);
    let settings = Settings {
        fallback_lang: cli.lang.clone(),
        delay: cli.delay,
        max_videos: cli.max_videos,
    };
    let mut runner = Runner {
        source: &mut source,
        http: &http,
        store: &mut store,
        channel: &listing.name,
        settings: &settings,
    };
    let stop = match runner.run(&pending) {
        Ok(stop) => stop,
        Err(error) => return fail(error, Outcome::Fatal),
    };

    let counts = count_statuses(&listing.videos, store.manifest());
    log_summary(&counts);
    match stop {
        Stop::Blocked => {
            tracing::error!(
                "YouTube blocked the requests. The state is saved. Wait some hours, then run the \
                 same command again, or use a proxy"
            );
            Outcome::Blocked
        }
        Stop::Finished => {
            let failed = counts.get(&Some(Status::Failed)).copied().unwrap_or(0)
                + counts.get(&Some(Status::Error)).copied().unwrap_or(0);
            if failed > 0 {
                Outcome::SomeFailed
            } else {
                Outcome::Complete
            }
        }
    }
}

fn pending_videos<'a>(
    videos: &'a [Video],
    manifest: &Manifest,
    retry_failed: bool,
) -> Vec<&'a Video> {
    videos
        .iter()
        .filter(|video| manifest.should_fetch(&video.id, retry_failed))
        .collect()
}

/// The number of videos for each status. `None` is pending.
fn count_statuses(videos: &[Video], manifest: &Manifest) -> BTreeMap<Option<Status>, usize> {
    let mut counts = BTreeMap::new();
    for video in videos {
        *counts.entry(manifest.status(&video.id)).or_insert(0) += 1;
    }
    counts
}

fn status_name(status: Option<Status>) -> &'static str {
    match status {
        None => "pending",
        Some(Status::Done) => "done",
        Some(Status::NoTranscript) => "no transcript",
        Some(Status::Unavailable) => "unavailable",
        Some(Status::Failed) => "failed",
        Some(Status::Error) => "error",
    }
}

fn summary_line(counts: &BTreeMap<Option<Status>, usize>) -> String {
    counts
        .iter()
        .map(|(status, count)| format!("{count} {}", status_name(*status)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn log_summary(counts: &BTreeMap<Option<Status>, usize>) {
    tracing::info!("Summary: {}", summary_line(counts));
}

fn print_dry_run(
    listing: &ChannelListing,
    manifest: &Manifest,
    channel_dir: &Path,
    retry_failed: bool,
) -> io::Result<()> {
    let mut out = io::stdout().lock();
    let handle = listing.handle.as_deref().unwrap_or("no handle");
    writeln!(
        out,
        "Channel: {} ({handle}, {})",
        listing.name, listing.channel_id
    )?;
    let removed = &listing.removed;
    writeln!(
        out,
        "Long videos: {} (removed: {} Shorts, {} live, {} too short)",
        listing.videos.len(),
        removed.shorts,
        removed.live,
        removed.too_short
    )?;
    writeln!(out, "Output folder: {}", channel_dir.display())?;
    writeln!(
        out,
        "Status: {}",
        summary_line(&count_statuses(&listing.videos, manifest))
    )?;
    let pending = pending_videos(&listing.videos, manifest, retry_failed);
    writeln!(out, "To fetch in this run: {}", pending.len())?;
    for video in pending {
        let duration = video
            .duration
            .map_or_else(|| "?".to_owned(), format_duration);
        writeln!(out, "  {}  {duration:>8}  {}", video.id, video.title)?;
    }
    Ok(())
}

fn format_duration(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use super::{format_duration, summary_line};
    use crate::store::Status;

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(Duration::from_secs(40)), "0:40");
        assert_eq!(format_duration(Duration::from_secs(2531)), "42:11");
        assert_eq!(format_duration(Duration::from_secs(14299)), "3:58:19");
    }

    #[test]
    fn summary_lists_pending_first() {
        let counts = BTreeMap::from([
            (Some(Status::Failed), 2),
            (None, 5),
            (Some(Status::Done), 10),
        ]);
        assert_eq!(summary_line(&counts), "5 pending, 10 done, 2 failed");
    }
}
