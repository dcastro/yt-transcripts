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

    /// Skip videos shorter than this duration (for example "2m").
    #[arg(long, value_parser = humantime::parse_duration)]
    pub(crate) min_duration: Option<Duration>,

    /// List the videos and the work to do. Send no transcript requests.
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Show each request in the log.
    #[arg(short, long)]
    pub(crate) verbose: bool,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
