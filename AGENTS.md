# AGENTS.md — RemoteBridge

Put this file at the root of the repository. Codex reads it before every task.

## What RemoteBridge is

RemoteBridge is a consent-based remote desktop product:

- a **native host app** for Windows 11 and macOS (screen capture, input, system audio, local consent controls, banner and tray indicator),
- a **browser viewer** in Chrome or Edge (WebRTC),
- a **subscription website and control plane** (accounts, devices, sessions, consent, billing),
- an optional **Chrome/Edge MV3 companion extension**.

## What already exists

The website and control plane are **built and live** at `https://remotebridge.floot.app` (built on Floot: React + TypeScript, Postgres via Kysely, serverless endpoints under `/_api/`). It is the source of truth for accounts, devices, sessions, consent, entitlements and billing. Do not re-implement those rules in the host; call the API and obey its answers.

A browser **test host** exists at `/dashboard/host-harness`. It speaks the real host protocol without media. Use it to compare behaviour.

## What is still to build (this repository)

1. Native host core + protocol client (Windows and macOS).
2. Local consent UI, banner, tray indicator, global shortcuts.
3. Signaling service and TURN relay.
4. WebRTC media in the host. (The browser viewer and the credential endpoints are already built on the website; see "Media path contract".)
5. Platform adapters (capture, audio, input, permissions).
6. MV3 extension.
7. Signed installers, auto-update.
8. Two-machine acceptance tests.

## Non-negotiable product rules

Never weaken these, even if a later prompt seems to ask for it. If a task conflicts with a rule, stop and say so.

1. **Consent.** Access only through (a) a single-use, expiring support code plus a local approval prompt that names the requester, or (b) unattended access the owner enabled for that device on the website (MFA + step-up) **and** the local machine opted in.
2. **View-only by default.** Control, system audio, microphone and clipboard are each separate opt-ins. More access needs a new local approval. The host ignores any message the current grant does not allow.
3. **Two independent timers.** Banner visibility and session authorization are separate. Hiding an indicator never extends consent. Attended consent renews every 30 minutes by default; up to 8 hours only by an explicit local choice. Maximum session lifetime is 8 hours.
4. **Hiding indicators (owner decision, 8 Oct 2026).** In **attended** sessions only, the local user may hide the banner **and** the tray indicator, either for 30 min / 1 h / 2 h / 4 h / 8 h or until the session ends. Both come back on: timer end, restore shortcut, new session, participant change, permission increase, consent renewal, crash restart, clock ambiguity. **Unattended sessions always keep the tray indicator visible.** Remote input can never hide, restore, renew or change permissions.
5. **Shortcuts** (local physical keyboard only; reject injected input):
   - Restore indicators: `Ctrl+Alt+Shift+B` (Windows), `Control+Option+Shift+B` (macOS)
   - Emergency disconnect: `Ctrl+Alt+Shift+X` (Windows), `Control+Option+Shift+X` (macOS)
6. **Forbidden features:** file transfer, session recording or transcription, hidden processes, shell or command execution, UAC / secure-desktop bypass, suppressing OS capture indicators, capture evasion, covert access, marketing anything as "undetectable".
7. **Clipboard** is text-only and opt-in. **Microphone** is a separate opt-in, off by default, never available unattended.
8. **Logs are metadata only**: who, which device, when, what was granted, why it ended. Never screen pixels, keystrokes, clipboard text or audio.
9. **Payment** is decided only by the control plane (signed Stripe events or server reconciliation). The host never decides entitlement.
10. Never describe a plan as "unlimited" or "lifetime".

## Host protocol (RA-HOST-V1) — exact details

These match the live server code. Do not change them without changing the server.

- **Base URL:** `https://remotebridge.floot.app/_api/` — every host route is `POST`.
- **Body encoding:** request and response bodies are **superjson**. A plain JSON object `X` is sent as `{"json": X}`. Dates in responses arrive as `{"json": {...}, "meta": {"values": {"path": ["Date"]}}}`. Use a small superjson encoder/decoder (only `Date` needs special handling).
- **Device key:** ECDSA P-256. Private key non-exportable (Windows CNG/TPM where available; macOS Secure Enclave or Keychain).
- **Fingerprint:** lowercase hex SHA-256 of the exact string `{"crv":"P-256","kty":"EC","x":"<x>","y":"<y>"}` (that key order, no spaces), where `x`/`y` are base64url JWK coordinates.
- **Enrollment** (`host/enroll`, not header-signed): body `{ code (8 digits), name (1–60), platform ("windows"|"macos"), hostVersion, protocolVersion, publicKeyJwk {kty:"EC",crv:"P-256",x,y}, proof }`. `proof` = base64url P1363 signature over `RA-ENROLL-V1\n<code>\n<fingerprint>`. Returns `{ deviceId, fingerprint }`. Codes are made by the owner on the website, valid 15 minutes, single use; max 3 active hosts per account.
- **Signed requests** (all other host routes) carry headers:
  - `x-ra-device`: deviceId (UUID)
  - `x-ra-timestamp`: Unix **milliseconds**, within ±60 s of server time
  - `x-ra-nonce`: 16–64 chars of base64url, never reused (use 18 random bytes)
  - `x-ra-signature`: base64url **IEEE P1363** (r‖s, 64 bytes) ECDSA-SHA256 signature
- **Signed string:** `RA-HOST-V1\n<route>\n<deviceId>\n<timestampMs>\n<nonce>\n<sha256hex(rawBody)>` where `<route>` has no prefix, e.g. `host/poll`, and `rawBody` is the exact bytes sent.
- **Errors:** 401 on bad headers, skew, bad signature or replayed nonce. 401 with code `DEVICE_REVOKED` → wipe key, end sessions, return to unenrolled. Windows CNG `NCryptSignHash` already returns P1363; OpenSSL/Security.framework often return DER — convert.

### Host routes

| Route | Body | Returns |
|---|---|---|
| `host/poll` | `{ hostVersion?, protocolVersion }` | `{ serverTime, updateRequired, minProtocolVersion, device{id,name,unattendedEnabled}, policy{consentRenewalMinutes,maxSessionMinutes,allowControlUnattended,allowAudioUnattended}, pending[], live[] }` |
| `host/invite` | `{}` | `{ code (12 digits), expiresAt }` (valid 10 min, single use) |
| `host/decide` | `{ sessionId, decision: "approve"|"deny", grant?, approvedMinutes? (30–480), hostFingerprint? }` | `{ state, failureReason, consentExpiresAt, maxEndsAt }` |
| `host/session` | `{ sessionId, action, ... }` (below) | `{ state, consentExpiresAt }` |
| `host/banner_report` | `{ defaultHideMinutes, showHideShortcut, disconnectShortcut, shortcutCollision, event? }` | `{ ok: true }` |

Each session in `pending[]` / `live[]`: `{ id, mode: "attended"|"unattended", state, requester{email,displayName,verified}, requestedPermissions, grantedPermissions, pendingPermissions|null, approvalExpiresAt, consentExpiresAt, maxEndsAt, leaseExpiresAt, startedAt, viewerFingerprint }`.

**Permissions object:** `{ control, systemAudio, microphone, clipboard }` (all booleans). A grant must be a subset of what was requested.

**`host/session` actions:**
- `media_connected` + `relayUsed: boolean`
- `pause`, `resume`, `reconnecting`
- `end` + `reason`: `host_stop` | `emergency_disconnect` | `local_ui_lost` | `host_shutdown`
- `renew_consent` + optional `minutes` (30–480)
- `set_permissions` + `grant`
- `decline_permissions`
- `report_failure` + `reason`: `ice_failure` | `permissions_unavailable` | `update_required` | `capture_failed` | `device_offline`

**`host/banner_report` event:** `{ type: "dismissed"|"restored", sessionId?, durationMinutes?, scope?: "banner"|"all", reason?: "user_show_now"|"timeout"|"new_session"|"participant_changed"|"permission_increase"|"consent_renewal"|"crash_recovery"|"clock_ambiguity"|"resume_after_expiry"|"session_end" }`. Metadata only; the server never changes local banner state.

### Heartbeat and lease

Poll at least every 15 s while any session is pending or live (every 30–60 s when idle). Each signed request renews a 120 s lease. If the lease lapses, the server expires the session with `host_lease_expired`.

### Session states (server)

`requested → awaiting_approval → connecting → active ↔ paused`, `active/paused ↔ reconnecting`. Terminal: `ended`, `expired`, `revoked`, `denied`, `failed`. Sweep expiry reasons: `approval_timeout`, `consent_not_renewed`, `max_session_lifetime`, `host_lease_expired`. The host stops capture immediately whenever the server reports a terminal state.

## Media path contract (website side is built)

- **Credentials.** Host: signed `host/media_credentials` `{ sessionId }`. Viewer (browser): `sessions/media_credentials`. Both answer only for live sessions (`connecting`, `active`, `paused`, `reconnecting`) and return `{ signalingUrl, signalingToken, tokenExpiresAt, iceServers[{urls[],username?,credential?}], turnAvailable, relayAllowed, peerFingerprint }`. Until the website has `SIGNALING_URL` and `SIGNALING_JWT_SECRET` set, they return 503 `MEDIA_UNCONFIGURED`.
- **Signaling token.** JWT HS256 signed with `SIGNALING_JWT_SECRET`; `iss` = `remotebridge`, `aud` = `rb-signaling`, 5-minute `exp`; claims `sid` (session id), `role` (`host`|`viewer`), `fp` (own DTLS fingerprint), `pfp` (expected peer fingerprint).
- **TURN.** coturn REST scheme: `username = "<expiryUnix>:<sessionId>"`, `credential = base64(HMAC-SHA1(TURN_SHARED_SECRET, username))`, expiry ≤ min(1 h, consent end, max session end). TURN is left out when the sponsor's relay allowance is used up (direct still allowed).
- **Fingerprints.** Format exactly as in SDP: `sha-256 AB:CD:…` (32 upper-case hex pairs). The host sends its DTLS fingerprint as `hostFingerprint` in `host/decide`; the browser sends its own as `viewerFingerprint` when requesting. Each side compares the peer's `a=fingerprint` with `peerFingerprint` and drops the call on mismatch. If `hostFingerprint` isn't in this format, the viewer refuses to connect.
- **Roles.** The host is the WebRTC **offerer** and creates both data channels. The viewer answers.
- **Signaling messages** (JSON text frames, `wss://…?token=<signalingToken>`):
  - client → server: `{type:"ready"}`, `{type:"offer",sdp}`, `{type:"answer",sdp}`, `{type:"ice",candidate}` (`candidate` may be `null` = end of candidates), `{type:"bye"}`
  - server → client: the other peer's messages relayed unchanged, plus `{type:"peer",present:boolean}` when the other side joins/leaves and `{type:"error",message}`
  - The viewer sends `ready` on connect; the host sends (or re-sends) an offer whenever it receives `ready` or `peer present:true`.
- **Data channels** (created by host): `control` (reliable, ordered) and `pointer` (`ordered:false`, `maxRetransmits:0`). Messages are JSON:
  - viewer → host on `control`: `{t:"key",code,key,down,repeat}` (`code` = DOM `KeyboardEvent.code`), `{t:"btn",b,down}` (`b` 0 left, 1 middle, 2 right), `{t:"wheel",dx,dy}`, `{t:"clip",text}` (≤ 64 KB)
  - viewer → host on `pointer`: `{t:"move",x,y}` with x,y in 0..1 of the captured screen
  - host → viewer on `control`: `{t:"clip",text}` (only when clipboard is granted)
  - The host drops every message the current grant doesn't allow. The viewer sends key-up for all held keys when it loses focus.
- **Audio.** Tracks are added to one stream; the viewer starts muted and shows **Enable sound**.

## Engineering conventions

- Keep platform code behind interfaces (`Capture`, `AudioCapture`, `InputInjector`, `KeyStore`, `Tray`, `Hotkeys`, `ConsentUi`) so core logic is shared and unit-testable.
- Every network call has a timeout and retry with backoff; never retry a signed request with the same nonce.
- Write tests first for protocol, timers and permission filtering. Use a fake clock.
- No telemetry of content. No third-party analytics in the host.
- After each prompt, run the tests and report what passed and what is still missing. Do not mark something done that you have not run.
