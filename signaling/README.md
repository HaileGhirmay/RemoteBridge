# signaling

WebSocket signaling relay (`rb-signaling`) plus TURN configuration.

## What it does

Follows `AGENTS.md`, "Media path contract":

- Clients connect with `?token=<signalingToken>` (HS256 JWT: `iss` `remotebridge`, `aud` `rb-signaling`, `exp`, `sid`, `role`, `fp`, `pfp`). Served at `/` and `/ws`.
- One room per session id, at most one host and one viewer; a second socket with the same role replaces the first (closed with code 4000).
- Relay only. Host may send `offer`, `ice`, `bye`; viewer may send `ready`, `answer`, `ice`, `bye`. Anything else is refused with `{"type":"error","message":…}`. Relayed messages are passed on unchanged.
- `{"type":"peer","present":true|false}` when the other side joins or leaves (and to a newcomer if the other side is already there).
- 64 KB per message, 50 messages/s per socket.
- A socket whose token expires is closed with code 4001; clients reconnect with fresh credentials.
- Logs hold session id, role, connect/disconnect time and the close reason. Never tokens, URLs, SDP, or ICE candidates.
- `GET /healthz` → `ok`.

## Run

```
SIGNALING_JWT_SECRET="$(openssl rand -base64 48)" cargo run -p rb-signaling --bin rb-signaling-server
```

`PORT` (default 8080), `BIND` (default 0.0.0.0), `RUST_LOG` (default info). TLS is terminated in front (see DEPLOY.md).

## Test

`cargo test -p rb-signaling`: unit tests plus an integration test with two scripted WebSocket clients (offer/answer/ice, presence, role limits, replacement, size and rate limits, expiry, bad tokens).

## Deploy

See [DEPLOY.md](DEPLOY.md). TURN: [turn/](turn/).
