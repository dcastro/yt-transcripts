# yt-transcripts: plan

A Rust command line tool. It gets the transcripts of all long videos of one YouTube channel. It
does not get the transcripts of Shorts. It writes each transcript to a file in a given folder.

## 1. Lessons from the previous attempt

The previous attempt used a Python YouTube MCP server and ad hoc scripts. YouTube blocked the IP
address. These are the causes and the lessons:

| What happened | Lesson for this tool |
| --- | --- |
| A script sent approx. 30 transcript requests quickly, then a scan of 180 more. There was no delay between requests. | Send transcript requests one at a time, with a long delay and random jitter. |
| After the block started, the script still sent all 180 requests. All of them failed with `IpBlocked`. | Stop the run at the first clear block signal. Do not continue to send requests. |
| The scripts did not save transcripts to disk. A new attempt had to request the same transcripts again. | Write each transcript to disk immediately. Keep a manifest. On a new run, skip all work that is done. |
| `yt-dlp` subtitle downloads also failed, with `HTTP 429`. | The block applies to the IP address, not to one library. A change of library does not help. Only time or a different IP address helps. |
| Requests for video data (the channel list, a video description) still worked during the block. | The channel list and the transcripts use different limits. A cheap channel list is safe to get on each run. |
| The MCP server hid the real error behind a different error. | Classify each error clearly. Show the real cause to the user. |

## 2. Scope

### Goals

- Accept a channel as `@handle`, a channel URL, or a channel ID (`UC…`).
- List all long videos of the channel. Exclude Shorts.
- Get the transcript of each video and write it to the output folder.
- Stay below the YouTube rate limit. If YouTube blocks the tool, stop safely.
- Continue from the last position on the next run (resumable).
- Support an HTTP or SOCKS5 proxy, for example a rotating residential proxy.

### Non-goals

- Download of video or audio.
- Speech-to-text for videos that have no captions.
- Search of the transcripts. Use `rg` or a different tool on the output folder.
- Login with a Google account or browser cookies. YouTube can ban the account. (This can be a
  later option, with a clear warning.)
- Parallel downloads. The tool sends one transcript request at a time, on purpose.

## 3. Command line interface

```text
yt-transcripts <CHANNEL> --out <DIR> [OPTIONS]

Arguments:
  <CHANNEL>               @handle, channel URL, or channel ID (UC…).
                          A name without "@" is used as a handle.

Options:
  -o, --out <DIR>         Output folder (required).
  -l, --lang <LIST>       Preferred languages, in order [default: en].
      --delay <DURATION>  Base delay between transcript requests [default: 10s].
      --jitter <FRAC>     Random change of the delay, +/- fraction [default: 0.5].
      --batch-size <N>    Number of transcripts before a long pause [default: 50].
      --batch-pause <D>   Length of the long pause [default: 5m].
      --cooldown <D>      Wait after the first HTTP 429, before one probe [default: 10m].
      --max-videos <N>    Stop after N new transcripts in this run.
      --proxy <URL>       http://, https:// or socks5:// proxy for all requests.
      --backend <NAME>    innertube | yt-dlp [default: innertube].
      --include-streams   Also get transcripts of past live streams.
      --min-duration <D>  Skip videos shorter than this duration.
      --retry-failed      Try again the videos that failed before with a transient error.
      --format <FMT>      md | json | both [default: both].
      --dry-run           List the videos and the work to do. Send no transcript requests.
  -v, --verbose           Show each request in the log.
```

Exit codes:

| Code | Meaning |
| --- | --- |
| 0 | All work is done. |
| 2 | Usage error, or the channel is not found. |
| 3 | YouTube blocked the tool. The run stopped. Run the same command again later. |
| 4 | The run is complete, but some videos failed. |
| 130 | The user stopped the run with Ctrl-C. The state is saved. |

## 4. Data flow

```text
CHANNEL ──► resolve ──► list videos ──► filter ──► compare with manifest
                                                          │
                                                          ▼
                           ┌──────────── for each video that is not done ◄────┐
                           │                                                  │
                           ▼                                                  │
                     throttle (delay + jitter, batch pause)                   │
                           │                                                  │
                           ▼                                                  │
                     fetch transcript ──► classify result                     │
                           │                    │                             │
                           │          block? ───┴──► save state, exit 3       │
                           ▼                                                  │
                     write files (atomic) ──► update manifest (atomic) ───────┘
```

## 5. Channel resolution and video list

### Method

Use `yt-dlp` as a subprocess:

```text
yt-dlp --flat-playlist --dump-single-json --proxy <PROXY> https://www.youtube.com/@handle/videos
```

Reasons:

- The `/videos` tab of a channel does not contain Shorts or live streams. Shorts are on the
  `/shorts` tab, and live streams are on the `/streams` tab. Thus, the tab is the main Shorts
  filter.
- `--flat-playlist` gets approx. 30 videos per request. A channel with 200 videos needs approx. 7
  requests. These requests did not get blocked in the previous attempt.
- The YouTube channel pages change often. `yt-dlp` gets frequent updates for these changes. A
  native Rust parser of these pages is fragile.

`yt-dlp` is already installed (`/usr/local/bin/yt-dlp`, version 2026.07.04). The tool checks
for `yt-dlp` at start and shows a clear error if it is missing.

### Filter

1. Use the `/videos` tab. With `--include-streams`, also use the `/streams` tab.
2. Remove each entry with a URL that contains `/shorts/` (safety check).
3. Remove upcoming and live-now entries. They have no transcript yet.
4. With `--min-duration`, remove shorter videos. Do not use a duration filter by default:
   Shorts can be up to 3 minutes long, and some long-form videos are shorter.

### Snapshot

Write the filtered list to `videos.json` in the channel folder. The tool gets a new list on each
run, so new uploads are included.

### Later option: YouTube Data API v3

If `YOUTUBE_API_KEY` is set, the tool can use the official API for the list:

- `channels.list?forHandle=@handle` gives the channel ID (1 quota unit).
- The playlist `UULF` + (channel ID without `UC`) contains only the long-form uploads.
- `playlistItems.list` gives 50 videos per request (1 quota unit), with the publish date.

The official API cannot give transcripts of videos that you do not own. Thus, the API does not
solve the transcript rate limit.

## 6. Transcript fetch

### Interface

```rust
pub trait TranscriptSource {
    fn fetch(&self, video: &VideoRef, langs: &[String]) -> Result<Transcript, FetchError>;
}
```

All network calls of a backend go through one shared HTTP client. This client applies the proxy,
the User-Agent and the cookie jar.

### Backend 1: `innertube` (default)

Use the same method as `youtube-transcript-api` 1.x. This method worked in the previous attempt
before the block.

1. Get the Innertube API key from one watch page. Do this one time per run, not one time per
   video.
2. For each video, send a `POST /youtubei/v1/player` request with an `ANDROID` client context.
3. Read `captions.playerCaptionsTracklistRenderer.captionTracks`.
4. Select a track. Use this order: a manual track in a preferred language, then an
   auto-generated track in a preferred language, then any manual track, then any auto-generated
   track.
5. Get the track `baseUrl` with `fmt=json3`. Parse the segments (start, duration, text).

This needs 2 requests per video. `yt-dlp` sends more requests per video. Fewer requests per video
means more videos before YouTube applies a limit.

Put the client name, the client version and the endpoint paths in one constants module. YouTube
changes these values sometimes.

First, examine the crate `yt-transcript-rs` (version 0.1.8). Use it only if these conditions are
true:

- It is maintained, and it works with the current YouTube site.
- It accepts our own `reqwest` client (for the proxy and the throttle).
- It gives errors that let us identify a block.

If one condition is false, write our own module. It is approx. 200 lines.

### Backend 2: `yt-dlp` (fallback)

Use this backend if YouTube changes its site and the `innertube` backend stops working.

```text
yt-dlp --skip-download --write-subs --write-auto-subs --sub-langs "en.*,en" \
       --sub-format json3 --proxy <PROXY> -o <TMP>/%(id)s.%(ext)s <VIDEO_URL>
```

The tool calls `yt-dlp` one time per video. The tool throttle controls the delay. Parse the
`json3` file with the same parser as backend 1. Parse the `yt-dlp` error text to classify the
error (for example `HTTP Error 429`).

## 7. Rate limit strategy

This is the most important part of the design.

### 7.1 One request at a time

The tool sends one transcript request at a time. There is no parallel work. Use a blocking HTTP
client (`reqwest::blocking`). The tool does not need an async runtime.

### 7.2 Delay with jitter

Before each video, wait `delay × (1 ± jitter)`. The default is 10 s ± 50 %, thus 5 s to 15 s.
The random value prevents a regular pattern of requests.

### 7.3 Batch pause

After each `--batch-size` transcripts (default 50), wait `--batch-pause` (default 5 minutes).

Example: a channel with 180 long videos takes approx. 180 × 11 s + 3 × 5 min ≈ 48 minutes. This
is slow, but the tool runs without supervision and it can resume.

### 7.4 Minimum requests

- Get the Innertube API key one time per run.
- Get the title, the duration and the upload date from the video list. Do not send a separate
  request for each video.
- Skip all videos that the manifest marks as done or as permanently unavailable.
- Do not request a transcript track list two times for one video in one run.

### 7.5 Error classes

| Signal | Class | Action |
| --- | --- | --- |
| No caption tracks, or transcripts disabled | Permanent | Record `no_transcript`. Do not try again. |
| Private, removed, members-only, or age-restricted video | Permanent | Record `unavailable`. Do not try again. |
| Timeout, connection reset, HTTP 5xx | Transient | Try again up to 3 times, with exponential backoff (30 s, 60 s, 120 s) and jitter. Then record `failed`. |
| First HTTP 429 in the run | Possible block | Obey `Retry-After` if it is present. If not, wait `--cooldown` (default 10 minutes). Then send one probe request. |
| HTTP 429 again after the cool-down | Block | Stop the run (circuit breaker). |
| "Sign in to confirm you're not a bot", redirect to `google.com/sorry`, reCAPTCHA page, `LOGIN_REQUIRED` on a public video | Block | Stop the run immediately (circuit breaker). |
| Unexpected response format | Parse error | Record `error` with the response saved to a debug file. Stop after 5 parse errors in sequence, because YouTube probably changed the site. |

### 7.6 Circuit breaker

When the tool identifies a block, it does these steps:

1. Stop all requests immediately.
2. Do not mark the current video as failed. It stays as pending.
3. Save the manifest.
4. Show a message: the number of transcripts done, the number pending, and the possible
   recoveries (wait some hours, or use `--proxy`).
5. Exit with code 3.

The previous attempt did not stop, and it sent 180 requests into the block. The circuit breaker
prevents this.

### 7.7 Proxy

`--proxy` applies to all HTTP requests and to all `yt-dlp` calls. A rotating residential proxy
(for example Webshare) gives a new IP address for each request. With a rotating proxy, the user
can set a shorter `--delay`.

### 7.8 HTTP client settings

- Use one realistic desktop User-Agent for the full run.
- Keep a cookie jar for the run. Set a consent cookie, so that the EU consent page does not
  appear.
- Set a request timeout of 30 s.
- With `--verbose`, log each request with the time, the URL path and the HTTP status. The user
  can then tune the delay values.

## 8. Storage

### Folder layout

```text
<OUT>/
  <channel-handle>/
    manifest.json
    videos.json
    transcripts/
      <video_id>_<title-slug>.md
      <video_id>_<title-slug>.json
    debug/                         # only for parse errors
```

The video ID is at the start of the file name. Thus, the name is unique and it is easy to find.
The tool sets the slug at the first write. If the title changes later, the file name does not
change.

### Markdown file

```markdown
---
video_id: biHf4BXm4G0
title: "How to Manual Like a Pro"
url: https://www.youtube.com/watch?v=biHf4BXm4G0
channel: Skate IQ
upload_date: 2025-03-14
duration_seconds: 1984
language: en
auto_generated: true
fetched_at: 2026-11-17T16:02:11Z
---

[00:00] Everyone wants to practice manuals from four wheels back to four wheels.
[00:04] And then whenever they hit the tail, that's a fall...
```

Each line has a timestamp. A search with `rg` then shows the position in the video directly.
This was necessary in the previous task.

### JSON file

The same metadata, and the full list of segments (`start`, `duration`, `text`). Other tools can
use this file.

### Manifest

`manifest.json` has one entry for each video ID:

```json
{
  "biHf4BXm4G0": {
    "status": "done",
    "file_stem": "biHf4BXm4G0_how-to-manual-like-a-pro",
    "language": "en",
    "attempts": 1,
    "last_error": null,
    "updated_at": "2026-11-17T16:02:11Z"
  }
}
```

Status values: `done`, `no_transcript`, `unavailable`, `failed`, `error`. A video that is not in
the manifest is pending.

### Atomic writes

Write each file to a temporary file in the same folder. Then rename it to the final name. Write
the transcript files first, then update the manifest. Thus, a crash or Ctrl-C cannot leave a
partial file. If a crash occurs between the two steps, the next run fetches that one video again.

### Resume rules

| Status | Default run | `--retry-failed` |
| --- | --- | --- |
| pending | fetch | fetch |
| `done` | skip | skip |
| `no_transcript`, `unavailable` | skip | skip |
| `failed`, `error` | skip | fetch |

## 9. Crates

| Crate | Use |
| --- | --- |
| `clap` (derive) | Command line parser |
| `reqwest` (`blocking`, `json`, `cookies`, `socks`, `rustls-tls`) | HTTP client and proxy |
| `serde`, `serde_json` | JSON parse and write |
| `thiserror` | Error types (`FetchError` and its classes) |
| `anyhow` | Errors in `main` |
| `tracing`, `tracing-subscriber` | Log output |
| `rand` | Jitter |
| `humantime` | Parse durations such as `10s` and `5m` |
| `indicatif` | Progress bar |
| `ctrlc` | Safe stop on Ctrl-C |
| `jiff` or `chrono` | Timestamps |
| `tempfile` | Atomic writes |
| `httpmock` or `mockito` (dev) | Mock HTTP server for tests |

Use the `rust-instructions` skill and `ctx7` docs for the current crate APIs at implementation.

## 10. Module layout

```text
src/
  main.rs            CLI wiring, exit codes
  cli.rs             clap definitions
  channel.rs         channel resolution and video list (yt-dlp subprocess)
  model.rs           VideoRef, Transcript, Segment
  transcript/
    mod.rs           TranscriptSource trait, track selection, json3 parser
    innertube.rs     backend 1
    ytdlp.rs         backend 2
    youtube_consts.rs  client name, client version, endpoint paths
  throttle.rs        delay, jitter, batch pause, backoff, circuit breaker
  error.rs           FetchError and classification
  store.rs           manifest, atomic writes, Markdown and JSON output
```

## 11. Implementation phases

### Phase 0: spike (live, minimum requests)

1. Send one request to check that the IP block has stopped. If it has not stopped, use a proxy or
   wait.
2. Run the `yt-dlp` list command on one channel. Save the output as a test fixture.
3. Fetch the transcript of one video with the `innertube` method. Save the player response and
   the `json3` response as test fixtures.
4. Examine `yt-transcript-rs`. Decide: use it, or write our own module.
5. Get one HTTP 429 response body, or a "not a bot" response, as a fixture if possible. If not,
   make fixtures from the `youtube-transcript-api` source code.

Send no more than 10 requests in this phase.

### Phase 1: list and dry run

- `cli.rs`, `channel.rs`, `model.rs`.
- `--dry-run` shows the list of long videos and the count of pending videos.
- Unit tests with the list fixture.

### Phase 2: transcript fetch and storage

- `transcript/innertube.rs`, track selection, `json3` parser.
- `store.rs`: Markdown, JSON, manifest, atomic writes, resume rules.
- Unit tests with the fixtures. Do not use the network in unit tests.

### Phase 3: throttle and circuit breaker

- `throttle.rs` and `error.rs`.
- Tests with a mock HTTP server: transient error then success, 429 then cool-down then success,
  429 two times then stop with exit code 3, "not a bot" page then stop immediately.
- Inject the clock and the sleep function, so that the tests run fast.
- Ctrl-C handling.

### Phase 4: proxy and `yt-dlp` backend

- `--proxy` for `reqwest` and for the `yt-dlp` subprocess.
- `transcript/ytdlp.rs`.

### Phase 5: real run

- Run on a small channel with `--max-videos 5`.
- Then run on the Skate IQ channel with default settings. Monitor the log.
- Do a second run. Make sure that it sends no transcript requests for videos that are done.

### Later

- Optional YouTube Data API v3 list backend (section 5).
- Optional `--cookies` with a clear warning about the risk to the account.
- A `search` subcommand over the stored transcripts.

## 12. Tests

- Unit tests: track selection, `json3` parser, timestamp format, slug, error classification,
  manifest resume rules.
- Mock HTTP tests: all rows of the table in section 7.5.
- One live test, marked `#[ignore]`. It fetches one transcript. Run it only by hand.
- Do not add tests that send many live requests.

## 13. Risks

| Risk | Mitigation |
| --- | --- |
| YouTube changes the Innertube API or the client values. | Values are in one module. The `yt-dlp` backend is the fallback. Stop after 5 parse errors in sequence. |
| YouTube requires a PO token for transcript requests. | The `yt-dlp` backend gets updates for this. Monitor `youtube-transcript-api` issues. |
| The block occurs even with the slow defaults. | The circuit breaker stops the run. The next run resumes. The user can increase `--delay` or use `--proxy`. |
| `yt-dlp` is missing or old. | Check at start. Show the installed version in `--verbose`. |
| A channel has thousands of videos. | Use `--max-videos` per run. Run again on following days. |

## 14. Open questions

1. Include past live streams by default, or only with `--include-streams`? (The plan: only with
   the flag.)
2. Which languages to prefer? (The plan: `en`, manual tracks before auto-generated tracks.)
3. Is the output format correct (Markdown with timestamps, and JSON)?
4. Do you have a proxy account, for example Webshare? If yes, the default delay can be shorter.
5. Do you want the YouTube Data API list backend in the first version?
