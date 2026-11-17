# yt-transcripts

A command line tool that downloads the transcripts of all long videos of a YouTube channel. It
does not download the transcripts of Shorts or live streams. It writes one Markdown file for each
video.

The tool is slow on purpose. It sends one transcript request at a time, with a random delay. If
YouTube blocks it, it stops and saves its state. When you run the same command again, it continues
from the last position. See [PLAN.md](PLAN.md) for the full design.

## Requirements

- Rust (2024 edition).
- [`yt-dlp`](https://github.com/yt-dlp/yt-dlp) in `PATH`. The tool uses it to get the video list.
  Keep it up to date (`yt-dlp -U`), because YouTube changes its site often.

You do not need a Google account or an API key.

## Install

```sh
cargo install --path .
```

## Usage

```sh
# Show the videos and the work to do. This sends no transcript requests.
yt-transcripts @Skateiq --out ~/transcripts --dry-run

# Download all transcripts.
yt-transcripts @Skateiq --out ~/transcripts

# Try 5 videos first.
yt-transcripts @Skateiq --out ~/transcripts --max-videos 5
```

The channel can be an `@handle`, a channel URL, or a channel ID (`UC…`).

With the default settings, a channel with 200 videos takes approximately 1 hour. Run
`yt-transcripts --help` for all options.

## Output

```text
<out>/<handle>/
  manifest.json      the state of each video
  videos.json        the video list of the last run
  transcripts/
    <video_id>_<title-slug>.md
  debug/             unexpected responses, if there are some
```

Each transcript file has YAML front matter and one line for each caption, with a timestamp. An
example (not a real video):

```markdown
---
video_id: abcdefghijk
title: "How to Manual"
url: https://www.youtube.com/watch?v=abcdefghijk
channel: "Skate IQ"
duration_seconds: 1984
language: "en"
auto_generated: true
fetched_at: 2026-11-17T16:02:11Z
---

[00:00] everyone wants to practice manuals
[00:02] from four wheels back to four wheels
```

To find a quote and its position in the video:

```sh
rg -i "four wheels back" ~/transcripts/Skateiq/transcripts
```

## Language

The tool gets the transcript in the original language of the video. It never uses a translated
track. Many videos have auto-dubbed audio tracks in other languages. The tool uses the original
audio track to find the original language. If the original language is not clear, it uses the
`--lang` language (default: English).

## Rate limits

YouTube blocks an IP address that sends too many transcript requests. The tool does these steps to
prevent a block:

- It waits 5 s to 15 s before each video, and 5 minutes after each 50 videos.
- It sends 2 requests for each video (`--backend innertube`).
- It skips all videos that are done.
- After the first HTTP 429, it waits 10 minutes and sends one probe request. If YouTube sends 429
  again, or a "not a bot" page, the tool stops with exit code 3.

If the tool stops with exit code 3, wait some hours and run the same command again. A block
usually stops after some hours.

### Proxy

If the blocks continue, use a rotating residential proxy:

```sh
yt-transcripts @Skateiq --out ~/transcripts --proxy "http://user:password@p.webshare.io:80"
```

Datacenter proxies (for example the free Webshare plan) usually do not work, because YouTube
blocks datacenter IP addresses. With a rotating proxy, you can use a shorter `--delay`.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | All work is done. |
| 1 | Fatal error, for example `yt-dlp` is missing, or 5 unexpected responses in sequence. |
| 2 | Usage error, or the channel is not found. |
| 3 | YouTube blocked the tool. The state is saved. Run the same command again later. |
| 4 | The run is complete, but some videos failed. Use `--retry-failed` to try them again. |
| 130 | Stopped by Ctrl-C. The state is saved. |

## If YouTube changes its site

If the tool stops with exit code 1 after 5 unexpected responses, look at the files in the `debug`
folder. Then try the yt-dlp backend:

```sh
yt-dlp -U
yt-transcripts @Skateiq --out ~/transcripts --backend yt-dlp --retry-failed
```

## Development

```sh
cargo test
cargo clippy --all-targets
```

The tests do not use the network. The test fixtures are in [tests/fixtures](tests/fixtures).
