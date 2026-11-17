//! yt-transcripts: download the transcripts of all long videos of a YouTube channel.

mod channel;
mod cli;
mod model;
mod ytdlp;

use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;

use crate::channel::{ChannelListing, ChannelRef, ListError};
use crate::cli::Cli;
use crate::ytdlp::YtDlp;

/// The result of a run, and its exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Complete,
    Fatal,
    Usage,
}

impl Outcome {
    fn exit_code(self) -> ExitCode {
        match self {
            Self::Complete => ExitCode::SUCCESS,
            Self::Fatal => ExitCode::from(1),
            Self::Usage => ExitCode::from(2),
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

fn run(cli: &Cli) -> Outcome {
    let channel = match ChannelRef::parse(&cli.channel) {
        Ok(channel) => channel,
        Err(error) => {
            tracing::error!("{error}");
            return Outcome::Usage;
        }
    };
    let ytdlp = match YtDlp::locate() {
        Ok(ytdlp) => ytdlp,
        Err(error) => {
            tracing::error!("{error}");
            return Outcome::Fatal;
        }
    };
    tracing::debug!("yt-dlp version {}", ytdlp.version());

    let listing = match channel::list_videos(&ytdlp, &channel, cli.min_duration) {
        Ok(listing) => listing,
        Err(error @ ListError::NotFound(_)) => {
            tracing::error!("{error}");
            return Outcome::Usage;
        }
        Err(error @ (ListError::YtDlp(_) | ListError::Parse(_))) => {
            tracing::error!("{error}");
            return Outcome::Fatal;
        }
    };
    let channel_dir = cli.out.join(listing.folder_name());
    let mut out = io::stdout().lock();
    let printed = print_listing(&mut out, &listing, &channel_dir).and_then(|()| {
        if cli.dry_run {
            print_pending(&mut out, &listing)
        } else {
            Ok(())
        }
    });
    if let Err(error) = printed
        && error.kind() != io::ErrorKind::BrokenPipe
    {
        tracing::error!("cannot write to the standard output: {error}");
        return Outcome::Fatal;
    }

    if cli.dry_run {
        return Outcome::Complete;
    }
    tracing::error!("The transcript download is not implemented yet. Use --dry-run");
    Outcome::Fatal
}

fn print_listing(
    out: &mut impl Write,
    listing: &ChannelListing,
    channel_dir: &Path,
) -> io::Result<()> {
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
    writeln!(out, "Output folder: {}", channel_dir.display())
}

fn print_pending(out: &mut impl Write, listing: &ChannelListing) -> io::Result<()> {
    writeln!(out, "Pending: {}", listing.videos.len())?;
    for video in &listing.videos {
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
    use std::time::Duration;

    use super::format_duration;

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(Duration::from_secs(40)), "0:40");
        assert_eq!(format_duration(Duration::from_secs(2531)), "42:11");
        assert_eq!(format_duration(Duration::from_secs(14299)), "3:58:19");
    }
}
