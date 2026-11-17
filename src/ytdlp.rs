//! A small wrapper around the `yt-dlp` program.

use std::io;
use std::process::Command;

const PROGRAM: &str = "yt-dlp";

#[derive(Debug, thiserror::Error)]
pub(crate) enum YtDlpError {
    #[error("yt-dlp is not installed or not in PATH ({0}). Install it, then try again")]
    NotFound(io::Error),
    #[error("cannot run yt-dlp: {0}")]
    Io(io::Error),
    #[error("yt-dlp failed: {stderr}")]
    Failed { stderr: String },
}

#[derive(Debug)]
pub(crate) struct YtDlp {
    version: String,
}

impl YtDlp {
    /// Find `yt-dlp` and get its version.
    pub(crate) fn locate() -> Result<Self, YtDlpError> {
        let output =
            Command::new(PROGRAM)
                .arg("--version")
                .output()
                .map_err(|error| match error.kind() {
                    io::ErrorKind::NotFound => YtDlpError::NotFound(error),
                    _ => YtDlpError::Io(error),
                })?;
        if !output.status.success() {
            return Err(YtDlpError::Failed {
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        Ok(Self { version })
    }

    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    /// Run `yt-dlp` with the arguments, and return its standard output.
    pub(crate) fn run(&self, args: &[&str]) -> Result<Vec<u8>, YtDlpError> {
        tracing::debug!("yt-dlp {}", loggable(args));
        let output = Command::new(PROGRAM)
            .args(args)
            .output()
            .map_err(YtDlpError::Io)?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(YtDlpError::Failed {
                stderr: error_lines(&output.stderr),
            })
        }
    }
}

/// The arguments for the log. The proxy URL can contain a password, so it is hidden.
fn loggable(args: &[&str]) -> String {
    let mut parts = Vec::with_capacity(args.len());
    let mut hide_next = false;
    for arg in args {
        parts.push(if hide_next { "<hidden>" } else { arg });
        hide_next = *arg == "--proxy";
    }
    parts.join(" ")
}

/// Keep only the `ERROR:` lines of the standard error, if there are some. yt-dlp also writes
/// warnings there, and they hide the real cause.
fn error_lines(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let errors: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("ERROR:"))
        .collect();
    if errors.is_empty() {
        text.trim().to_owned()
    } else {
        errors.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::{error_lines, loggable};

    #[test]
    fn log_hides_the_proxy() {
        let args = ["-J", "--proxy", "http://user:secret@proxy:80", "--", "url"];
        assert_eq!(loggable(&args), "-J --proxy <hidden> -- url");
    }

    #[test]
    fn error_lines_keeps_only_errors() {
        let stderr = b"WARNING: [youtube] No supported JavaScript runtime\n\
                       ERROR: [youtube:tab] @nope: This channel does not exist.\n";
        assert_eq!(
            error_lines(stderr),
            "ERROR: [youtube:tab] @nope: This channel does not exist."
        );
    }

    #[test]
    fn error_lines_keeps_all_text_without_errors() {
        assert_eq!(error_lines(b"  something odd \n"), "something odd");
    }
}
