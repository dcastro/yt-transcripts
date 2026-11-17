# Test fixtures

These files come from the Phase 0 spike on 2026-11-17. The tests use them. The tests do not use
the network.

| File | Source |
| --- | --- |
| `ytdlp_channel_videos.json` | Real. `yt-dlp --flat-playlist --dump-single-json` on `@Skateiq/videos`. Trimmed: no thumbnails. |
| `player_dubbed.json` | Real. Innertube `player` response (`ANDROID` client) for `WlmQHrgV7xA`, a video with auto-dubbed audio tracks. Trimmed. Each `baseUrl` is replaced with a fake URL without an IP address or a signature. |
| `ytdlp_video_dubbed.json` | Real. `yt-dlp -J --skip-download` for the same video. Trimmed to five caption languages. The URLs are fake. |
| `google_sorry_429.html` | Real. The body of the HTTP 429 response from `timedtext` while YouTube blocked the IP address. |
| `timedtext_asr.json3` | Synthetic. The `json3` format of an auto-generated track. YouTube blocked `timedtext` during the spike, so a real sample was not available. |
| `timedtext_manual.json3` | Synthetic. The `json3` format of a manual track. |

Do not add fixtures that contain an IP address, a signature or an API key.
