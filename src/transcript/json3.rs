//! Parser for the `json3` caption format.

use std::time::Duration;

use serde::Deserialize;

use crate::error::FetchError;
use crate::model::Segment;

#[derive(Debug, Deserialize)]
struct Json3 {
    events: Vec<Event>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    t_start_ms: Option<u64>,
    #[serde(default)]
    segs: Vec<Seg>,
}

#[derive(Debug, Deserialize)]
struct Seg {
    utf8: Option<String>,
}

/// Parse a `json3` body into segments. Events without text (window setup events and line
/// breaks of auto-generated tracks) are removed.
pub(crate) fn parse(body: &str) -> Result<Vec<Segment>, FetchError> {
    if body.trim().is_empty() {
        return Err(FetchError::unexpected(
            "the transcript response is empty",
            None,
        ));
    }
    let json3: Json3 = serde_json::from_str(body).map_err(|error| {
        FetchError::unexpected(
            format!("the transcript is not valid json3: {error}"),
            Some(body),
        )
    })?;
    let segments = json3
        .events
        .into_iter()
        .filter_map(|event| {
            let text: String = event
                .segs
                .iter()
                .filter_map(|seg| seg.utf8.as_deref())
                .collect();
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let start = Duration::from_millis(event.t_start_ms.unwrap_or(0));
            (!text.is_empty()).then_some(Segment { start, text })
        })
        .collect();
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::parse;
    use crate::error::FetchError;
    use crate::model::Segment;

    fn segment(ms: u64, text: &str) -> Segment {
        Segment {
            start: Duration::from_millis(ms),
            text: text.to_owned(),
        }
    }

    #[test]
    fn parses_auto_generated_tracks() {
        let segments = parse(include_str!("../../tests/fixtures/timedtext_asr.json3")).unwrap();
        assert_eq!(
            segments,
            [
                segment(160, "everyone wants to practice manuals"),
                segment(2480, "from four wheels back to four wheels"),
                segment(4480, "[Music]"),
                segment(3_725_000, "and then whenever they hit the tail"),
            ]
        );
    }

    #[test]
    fn parses_manual_tracks() {
        let segments = parse(include_str!("../../tests/fixtures/timedtext_manual.json3")).unwrap();
        assert_eq!(
            segments,
            [
                segment(
                    1200,
                    "Everyone wants to practice manuals from four wheels back to four wheels."
                ),
                segment(4200, "And then whenever they hit the tail,"),
                segment(7700, "that's a fall."),
            ]
        );
    }

    #[test]
    fn rejects_empty_and_bad_bodies() {
        assert_eq!(
            parse("  ").unwrap_err(),
            FetchError::unexpected("the transcript response is empty", None)
        );
        let FetchError::Unexpected { message, body } = parse("<html>").unwrap_err() else {
            panic!("expected an unexpected-response error");
        };
        assert!(message.starts_with("the transcript is not valid json3"));
        assert_eq!(body.as_deref(), Some("<html>"));
    }
}
