//! YouTube values that change sometimes. Keep them all here (PLAN.md, section 6.2).
//!
//! Source: `youtube-transcript-api` 1.2.5, checked in the Phase 0 spike on 2026-11-17.

pub(crate) const YOUTUBE_BASE_URL: &str = "https://www.youtube.com";
pub(crate) const WATCH_PATH: &str = "/watch";
pub(crate) const PLAYER_PATH: &str = "/youtubei/v1/player";
pub(crate) const CLIENT_NAME: &str = "ANDROID";
pub(crate) const CLIENT_VERSION: &str = "20.10.38";

/// The watch page has this key in a JavaScript object: `"INNERTUBE_API_KEY":"AIza…"`.
pub(crate) const API_KEY_MARKER: &str = "\"INNERTUBE_API_KEY\"";
/// The EU cookie consent page has a form with this action.
pub(crate) const CONSENT_FORM_MARKER: &str = "action=\"https://consent.youtube.com/s\"";
/// A track URL with this parameter needs a PO token.
pub(crate) const PO_TOKEN_MARKER: &str = "&exp=xpe";
/// The playability reason when YouTube thinks that the client is a bot.
pub(crate) const BOT_REASON_MARKER: &str = "not a bot";
