//! Command line arguments.

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;

/// Download the transcripts of all long videos of a YouTube channel.
///
/// The tool sends one transcript request at a time, with a delay. If YouTube blocks the tool, it
/// stops and saves its state. Run the same command again later to continue.
#[derive(Debug, Parser)]
#[command(version, about)]
pub(crate) struct Cli {
    /// The channel: an @handle, a channel URL, or a channel ID (UC…). A name without "@" is used
    /// as a handle.
    pub(crate) channel: String,

    /// Output folder. The tool makes one subfolder for each channel.
    #[arg(short, long)]
    pub(crate) out: PathBuf,

    /// Language to use when the original language of a video is not clear.
    #[arg(long, default_value = "en")]
    pub(crate) lang: String,

    /// Base delay between videos.
    #[arg(long, default_value = "10s", value_parser = humantime::parse_duration)]
    pub(crate) delay: Duration,

    /// Random change of the delay, as a fraction from 0 to 1 (0.5 gives 5 s to 15 s for a 10 s
    /// delay).
    #[arg(long, default_value_t = 0.5, value_parser = parse_fraction)]
    pub(crate) jitter: f64,

    /// Number of videos before a long pause. 0 means no long pause.
    #[arg(long, default_value_t = 50)]
    pub(crate) batch_size: usize,

    /// Length of the long pause.
    #[arg(long, default_value = "5m", value_parser = humantime::parse_duration)]
    pub(crate) batch_pause: Duration,

    /// Wait after the first HTTP 429, before one probe request. YouTube's Retry-After value
    /// replaces it, if YouTube gives one.
    #[arg(long, default_value = "10m", value_parser = humantime::parse_duration)]
    pub(crate) cooldown: Duration,

    /// Stop after this number of videos in this run.
    #[arg(long)]
    pub(crate) max_videos: Option<usize>,

    /// Skip videos shorter than this duration (for example "2m").
    #[arg(long, value_parser = humantime::parse_duration)]
    pub(crate) min_duration: Option<Duration>,

    /// Try again the videos that failed before with a temporary or unexpected error.
    #[arg(long)]
    pub(crate) retry_failed: bool,

    /// List the videos and the work to do. Send no transcript requests.
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Show each request in the log.
    #[arg(short, long)]
    pub(crate) verbose: bool,
}

fn parse_fraction(text: &str) -> Result<f64, String> {
    let value: f64 = text
        .parse()
        .map_err(|error| format!("not a number: {error}"))?;
    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err("the value must be from 0 to 1".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::{Cli, parse_fraction};

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn jitter_must_be_a_fraction() {
        assert_eq!(parse_fraction("0.25"), Ok(0.25));
        assert_eq!(parse_fraction("1"), Ok(1.0));
        assert!(parse_fraction("1.5").is_err());
        assert!(parse_fraction("-0.1").is_err());
        assert!(parse_fraction("abc").is_err());
    }
}
