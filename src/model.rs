//! Data types that the modules share.

use std::time::Duration;

/// One long video of the channel, from the video list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Video {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) duration: Option<Duration>,
}
