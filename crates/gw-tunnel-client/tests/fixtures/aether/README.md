# Aether wire fixtures

Captured 2026-06-15 from `aether-proxy` at Aether commit `0a79ccd066ed362effbefbd1602e8bba4a0f9687` (tag `v0.0.8`).

Reference: https://gitlab.ailinkmax.com/ailinkmax/Aether @ `v0.0.8`.

Capture harness: `/tmp/aether-capture/capture_aether_fixtures.py` (ad-hoc, not committed). The harness ran a mock Aether control plane, accepted the proxy WebSocket at `/api/internal/proxy-tunnel`, sent one `GET http://1.1.1.1/` request over the tunnel, and recorded the first response and heartbeat frames returned by the real proxy binary.

Per-file content:
- `request_headers.bin`  — one `MsgType::RequestHeaders` frame (header + JSON body)
- `request_body.bin`     — one empty `MsgType::RequestBody` frame with `END_STREAM`
- `response_headers.bin` — one gzip-compressed `MsgType::ResponseHeaders` frame
- `response_body.bin`    — one uncompressed `MsgType::ResponseBody` frame
- `stream_end.bin`       — one `MsgType::StreamEnd` frame
- `heartbeat_data.bin`   — one `MsgType::HeartbeatData` frame
- `heartbeat_data.json`  — JSON payload extracted from `heartbeat_data.bin`
