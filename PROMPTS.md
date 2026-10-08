# RemoteBridge — Codex prompt pack

**How to use this**

1. Create an empty Git repository and copy `AGENTS.md` to its root. Codex reads it on every task, so the rules and protocol details don't need repeating in each prompt.
2. Paste the prompts **one at a time, in order**. Wait for each to finish, run the tests, review the diff, commit, then go on to the next one.
3. Part A covers the work that is still to build. Part B describes the website that is already live. Use Part B only if you ever move the website off Floot, or need Codex to understand it.
4. Each prompt ends with "Done when". Don't accept the work until those checks pass on a real machine.

**Stack choice (decide before Prompt 01).** This pack defaults to a **Rust** shared core with thin native shells (Windows: Rust + `windows` crate; macOS: Swift app calling the Rust core through a C ABI). If you choose C++ (Windows) + Swift (macOS) instead, replace "Rust" in Prompt 01 and keep everything else.

---

## Part A — Still to build

### Prompt 01 — Repository and shared host core

```
Read AGENTS.md first. Set up a monorepo for the RemoteBridge native host.

Layout:
- core/            Rust library: protocol client, session manager, consent and banner timers, permission filter. No platform code.
- platform/windows Rust crate implementing the platform traits for Windows 11.
- platform/macos   Swift package + Rust staticlib bridge (C ABI via cbindgen) for macOS 13+.
- signaling/       (empty for now)
- viewer/          (empty for now)
- extension/       (empty for now)
- tools/           scripts

In core/, define traits: KeyStore, Capture, AudioCapture, MicCapture, InputInjector, Tray, Banner, Hotkeys, ConsentUi, Clock (wall + monotonic). Provide fake implementations for tests.

Add CI (GitHub Actions) that builds core on Windows and macOS and runs `cargo test`. Add rustfmt + clippy with warnings as errors.

Done when: `cargo test` passes on both OSes in CI and the fakes compile.
```

### Prompt 02 — RA-HOST-V1 protocol client

```
Read AGENTS.md, section "Host protocol (RA-HOST-V1)". Implement it in core/src/protocol/.

1. superjson codec: encode a serde value as {"json": value}; decode responses, turning paths listed in meta.values with ["Date"] into timestamps. Unit-test with real samples (write fixtures by hand from AGENTS.md).
2. Fingerprint: lowercase hex SHA-256 of {"crv":"P-256","kty":"EC","x":"..","y":".."} in exactly that order.
3. Signing: build "RA-HOST-V1\n<route>\n<deviceId>\n<tsMs>\n<nonce>\n<sha256hex(rawBody)>"; sign via the KeyStore trait; output base64url P1363 (64 bytes). Add a DER→P1363 converter with tests.
4. Nonce: 18 random bytes, base64url (24 chars). Never reuse a nonce, even on retry — a retry is a new signed request.
5. HTTP client (reqwest + rustls) with 10 s timeout, exponential backoff with jitter, and a typed error for 401 DEVICE_REVOKED.
6. Typed request/response structs for host/enroll, host/poll, host/invite, host/decide, host/session (all actions), host/banner_report, exactly as in AGENTS.md.
7. Enrollment: proof = sign("RA-ENROLL-V1\n<code>\n<fingerprint>") with the same key.
8. A CLI `rb-hostctl` for testing: `enroll <code>`, `poll`, `invite`, `decide <id> approve|deny`, `end <id>`. Use a file-based software key ONLY in this CLI, behind a feature flag "dev-key".

Test against the live server: generate an enrollment code on https://remotebridge.floot.app/dashboard/devices, run `rb-hostctl enroll <code>`, then `rb-hostctl poll`.

Done when: unit tests pass; the CLI enrolls, polls, creates an invite; a replayed request gets 401; revoking the device on the website makes the next poll return DEVICE_REVOKED and the CLI deletes its key.
```

### Prompt 03 — Secure device keys

```
Implement the KeyStore trait for real devices.

Windows: CNG with the Microsoft Platform Crypto Provider (TPM) when available, else Microsoft Software Key Storage Provider; key non-exportable; per-machine key owned by the service account; NCryptSignHash output is already P1363.
macOS: Secure Enclave key (kSecAttrTokenIDSecureEnclave) when available, else Keychain key with kSecAttrIsPermanent and non-extractable; SecKeyCreateSignature returns DER — convert to P1363.

Both: export only the public key as JWK x/y (base64url, 32 bytes each). Provide `wipe()` used on DEVICE_REVOKED and uninstall.

Done when: enroll + poll work on both OSes with hardware-backed keys where available; the private key cannot be exported (add a test that tries and fails); wipe() removes it.
```

### Prompt 04 — Session manager, heartbeat and consent

```
In core/src/session/, build the host session manager.

- Poll loop: every 15 s while any session is pending or live, every 45 s when idle. Each signed call renews the server lease (120 s). Use the monotonic clock for scheduling.
- Keep a local mirror of pending[] and live[] from host/poll. The server is the source of truth: if a session disappears or turns terminal (ended, expired, revoked, denied, failed), stop capture and input for it at once.
- Attended request: show ConsentUi with requester displayName + email, mode, and each requested permission as a separate checkbox, all unchecked except "view". The user can approve a subset. Consent window choice: 30 min (default), 1 h, 2 h, 4 h, 8 h — longer than 30 min only by explicit choice. Send host/decide with decision, grant, approvedMinutes and hostFingerprint (DTLS fingerprint, filled in Prompt 08; send none until then).
- Unattended request: auto-approve only if device.unattendedEnabled from the server AND a local opt-in flag set in the host settings by a local admin. Never grant microphone unattended. Grant control/audio only if policy.allowControlUnattended / allowAudioUnattended.
- Consent renewal: 2 minutes before consentExpiresAt, show a local prompt "Keep sharing with <name>?" → host/session renew_consent. If ignored, let it expire; the server ends the session.
- Permission escalation: when pendingPermissions is set, show a local prompt; approve → set_permissions, decline → decline_permissions.
- Permission filter: a pure function deciding if an incoming viewer message is allowed by the current grant. Drop anything not allowed and count it (metadata only).
- Emergency disconnect: end with reason emergency_disconnect, stop all capture locally first, then tell the server.
- Support code: a "Get help" button calls host/invite and shows the 12-digit code and a 10-minute countdown.
- Revoked: on DEVICE_REVOKED, end everything, wipe the key, show "This device was removed from your account".

Unit-test with the fake clock and a fake server: approval timeout, consent expiry, renewal, escalation approve/decline, unattended rules, lease loss, revoke.

Done when: tests pass; with `rb-hostctl`-style manual runs against the live site, a viewer request from the website appears in the local prompt and approve/deny show up in the website's session detail page.
```

### Prompt 05 — Banner, tray and shortcuts

```
Implement indicator logic in core/src/indicators/ as a pure state machine, then the platform UIs.

Port this exact logic (it is the website's tested reference, helpers/bannerTimer):
- Dismissal record: sessionId, participantKey, permsKey (comma list of granted scopes), consentKey (changes on each renewal), scope "banner"|"all", durationMinutes, wallDeadline, monoDeadline.
- dismiss(ctx, minutes in {30,60,120,240,480}, scope) and dismissAll(ctx, sessionEndsAtWall) (scope "all").
- evaluate(d, ctx) → show with a reason, in this order: no session → session_end; no dismissal → no_dismissal; different session → new_session; different participant → participant_changed; any new permission → permission_increase; consentKey changed → consent_renewal; monoDeadline missing (after restart) → crash_recovery; |wallLeft − monoLeft| > 120 s → clock_ambiguity; deadline passed → timeout, or resume_after_expiry if overshoot > 5 s. Otherwise hidden, trayHidden = (scope == "all").
- persist() writes the record with monoDeadline = null, so any restart shows the indicators.

Rules on top:
- Attended sessions: both the timed hide and "Hide until session ends" use scope "all" (banner AND tray hidden).
- Unattended sessions: hiding is allowed for the banner only; the tray is always visible.
- Only local input can hide. First time ever, show: "The banner and tray icon will be hidden. The person connected can still see your screen. Press <restore shortcut> to show them again."
- Report every dismiss/restore to host/banner_report with type, sessionId, durationMinutes, scope, reason.

Hotkeys:
- Windows: RegisterHotKey for Ctrl+Alt+Shift+B (restore) and Ctrl+Alt+Shift+X (emergency disconnect). Also a low-level keyboard hook that ignores events with LLKHF_INJECTED so remote-injected keys cannot trigger them.
- macOS: CGEventTap; accept only events whose source state is kCGEventSourceStateHIDSystemState and not posted by our own injector (tag our injected events with a private eventSourceUserData value and reject those).
- If registration fails, set shortcutCollision = true, warn the user and keep the tray menu items "Show indicators" and "Disconnect now" as fallback.

UI:
- Banner: always-on-top, click-through except its buttons, on every display, text "<name> can see your screen" + list of granted permissions + buttons Hide…, Disconnect.
- Tray / menu-bar: icon changes while a session is live; menu shows who is connected, time left on consent, Show indicators, Disconnect now.
- Excluded from our own capture (WDA_EXCLUDEFROMCAPTURE on Windows; SCContentFilter exclusion on macOS) so the viewer doesn't see a banner-within-banner. This hides it from OUR stream only — never from OS indicators.

Done when: unit tests cover every reason above (copy the cases from the website's bannerTimer.spec), and on both OSes a manual run shows hide/restore working, injected shortcut keys are ignored, and unattended sessions keep the tray.
```

### Prompt 06 — Control plane additions for media — ALREADY DONE

Built on the website on 8 Oct 2026: `host/media_credentials`, `sessions/media_credentials`, viewer DTLS key binding. The exact contract is in AGENTS.md under "Media path contract". Nothing to paste into Codex. When the signaling and TURN servers exist (Prompt 07), add these secrets to the website: `SIGNALING_URL`, `SIGNALING_JWT_SECRET`, `TURN_URLS`, `TURN_SHARED_SECRET`, optional `STUN_URLS`.

### Prompt 07 — Signaling service and TURN

```
In signaling/, build a small WebSocket signaling server (Rust axum or Node + ws; pick one, keep it small).

Follow AGENTS.md "Media path contract" exactly — the website and browser viewer already use it.
- Client connects with ?token=<signalingToken>. Verify HS256 with SIGNALING_JWT_SECRET, iss "remotebridge", aud "rb-signaling", exp; read sid/role/fp/pfp.
- One room per sid, max one host + one viewer (a second socket with the same role replaces the first).
- Relay only: ready, offer, answer, ice, bye — from the host only offer/ice/bye, from the viewer only ready/answer/ice/bye. Send {type:"peer",present} to the other side on join/leave and {type:"error",message} on refusals. Max 64 KB per message, 50 msgs/s per socket. Drop anything else.
- No content logging. Log: sid, role, connect/disconnect time, close reason.
- Close both sockets when the token expires; clients reconnect with fresh credentials.
- Health endpoint /healthz. Dockerfile. Deploy notes for Fly.io or a small VM with TLS.

TURN: provide a coturn config (use-auth-secret, static-auth-secret from TURN_SHARED_SECRET, realm remotebridge, TLS on 443 and 5349, UDP 3478, deny private IP ranges as peers, total-quota and bps-capacity limits). Provide docker-compose and a firewall checklist.

Done when: integration test with two scripted clients exchanges offer/answer/ice; a bad or expired token is refused; coturn accepts credentials minted by Prompt 06 and rejects tampered ones (test with turnutils_uclient).
```

### Prompt 08 — WebRTC media in the host

```
Add media to the host using libwebrtc (via the `webrtc` crate or a libwebrtc binding; on macOS you may use the native WebRTC framework from Swift). 

Follow AGENTS.md "Media path contract" exactly — the browser viewer on the website is already built against it.
- Generate the DTLS certificate BEFORE host/decide and send its fingerprint as hostFingerprint in the format "sha-256 AB:CD:…". Use that same certificate for the peer connection.
- After approval, call host/media_credentials, connect to signaling, create the RTCPeerConnection with the returned iceServers. The host is the offerer: create the "control" and "pointer" data channels, then send an offer whenever the viewer sends "ready" or the server sends {type:"peer",present:true}.
- Check the viewer's a=fingerprint in the answer equals peerFingerprint from host/media_credentials; if not, close and report_failure ice_failure.
- Video: one track from the Capture trait (H.264 hardware encoder where available, VP8 fallback), adaptive bitrate, 30 fps cap.
- Audio: system audio as one Opus track only if systemAudio granted; microphone as a second track only if microphone granted. Stop a track at once when a permission is removed.
- Data channels: "control" (reliable, ordered) for keyboard, buttons, clipboard text, control messages; "pointer" (ordered=false, maxRetransmits=0) for pointer moves. Every message passes the permission filter from Prompt 04 before reaching InputInjector or the clipboard.
- Clipboard: text only, max 64 KB, only if clipboard granted, both directions, never logged.
- Report media_connected with relayUsed (true if the selected candidate pair is relay). Report reconnecting / resume on ICE state changes. On failure, report_failure ice_failure.
- Stop all media when the server says the session is terminal or the lease is lost.

Done when: a viewer (Prompt 09) sees the screen, hears system audio when granted, control works only when granted, and relayUsed is true when UDP is blocked.
```

### Prompt 09 — Browser viewer — ALREADY DONE

Built into the website's session page (components/RemoteViewer): video, Enable sound, control and pointer channels only when control is granted, text clipboard buttons, full screen with keyboard lock, host fingerprint check, and an honest "Screen streaming isn't switched on yet" message until the media services are connected. Nothing to paste into Codex. Test it from Prompt 08 onward.

### Prompt 10 — Platform adapters

```
Implement the platform traits.

Windows 11:
- Capture: Windows.Graphics.Capture (per-monitor, cursor included), D3D11 textures to the encoder without CPU copies where possible.
- System audio: WASAPI loopback. Microphone: WASAPI capture.
- Input: SendInput with scan codes; tag injected events (dwExtraInfo) so our own hook can recognize them.
- Secure desktop (UAC prompt, Ctrl+Alt+Del, lock screen): capture fails there — show a placeholder frame "The person at this computer needs to respond", never try to bypass.
- Runs as a per-user app with a small per-machine service only for unattended start at login; no hidden processes; visible in Task Manager with the product name.

macOS 13+:
- Capture: ScreenCaptureKit (SCStream), excluding our banner window.
- System audio: ScreenCaptureKit audio. Microphone: AVFoundation, with its permission prompt.
- Input: CGEvent posting; needs Accessibility permission.
- Missing Screen Recording or Accessibility permission → report_failure permissions_unavailable and show a guide to System Settings. Never work around it.
- OS recording indicator must remain visible; do not hide it.

Done when: capture, audio and input work on both OSes; missing permissions are reported correctly; the secure desktop shows the placeholder.
```

### Prompt 11 — Chrome/Edge MV3 extension

```
In extension/, build a Manifest V3 extension. It is optional: the viewer must work without it.

- Popup: sign-in state (opens the website to sign in), list of the user's devices with online status (website API with the user's session cookie, host permission only for https://remotebridge.floot.app/*), "Get support" and "Connect" buttons that open the website pages.
- Permissions: storage, and host_permissions for the website only. No <all_urls>, no tabs content scripts, no remote code.
- Optional native messaging host "com.remotebridge.host" to show whether the native host is installed and open its window. The native side only answers {installed, version} and "open"; nothing else.
- Package for the Chrome Web Store and Edge Add-ons with privacy disclosures.

Done when: extension passes Chrome Web Store checks locally (no warnings for broad permissions) and works with and without the native host installed.
```

### Prompt 12 — Installers, signing and updates

```
Windows: MSIX (or WiX MSI) signed with an EV certificate or Azure Trusted Signing. Per-user install by default; optional per-machine service for unattended access, which asks a local admin. Uninstall removes the service, tray app, key (KeyStore.wipe) and settings.
macOS: .pkg or .dmg, Developer ID signed, notarized, Hardened Runtime; entitlements only for what is needed; first-run guide for Screen Recording, Accessibility, Microphone.
Updates: check a signed manifest (Ed25519) from the release channel; download, verify signature and hash, install on next restart when no session is live; keep one previous version for rollback. Respect updateRequired from host/poll: stop accepting new sessions and prompt to update.
CI: build, sign, notarize and publish artifacts on tagged releases. Then update the website's release_channels rows with real versions and download URLs.

Done when: a fresh machine installs, enrolls, runs a session, updates to a newer build and uninstalls cleanly on both OSes.
```

### Prompt 13 — Acceptance tests

```
Create tests/acceptance/ with a written checklist and automated parts where possible. Run on one Windows 11 and one macOS machine, on two different networks, against production.

1. Enroll with a valid code; reused or expired code rejected.
2. Replayed request and a request with a 61 s clock offset rejected.
3. Revoke on the website: next poll DEVICE_REVOKED, live session ends, key wiped.
4. Support code works once and expires after 10 minutes.
5. Approval prompt names the requester; deny ends the request (state denied).
6. View-only by default; control, system audio, microphone, clipboard each need explicit approval.
7. A grant larger than requested is rejected by the server.
8. Killing the host ends the session within 120 s (host_lease_expired).
9. Consent ends at the approved time unless renewed locally; viewer cannot extend.
10. Timed hide and hide-until-end hide banner and tray (attended); every restore reason brings them back; unattended keeps the tray.
11. Shortcuts work from the physical keyboard; injected shortcut keys are ignored.
12. Unattended never grants microphone.
13. No plan → refused (402); one-time pass activates at the first approved session and lasts 24 h.
14. TURN fallback works with UDP blocked; relayUsed = true in usage.
15. DTLS fingerprint mismatch drops the call.
16. Search all logs (host, signaling, TURN, website) for screen data, keystrokes, clipboard text, audio: none found.
17. Emergency disconnect works while indicators are hidden.

Produce a report table: test, Windows result, macOS result, evidence.
```

---

## Part B — What the website already does (for reference)

These describe the live website so Codex understands it, or can rebuild it on another stack if you ever leave Floot. The real source code is in the Floot project; ask Claude to export it if you need the files.

### B1 — Product shell and design

```
Build the RemoteBridge website in React + TypeScript with a Postgres database.
Pages: Home, Features, Pricing, Download (support matrix; marks builds as "feasibility testing" until real installers exist), Help, Sign in, Register, Forgot password, Reset password, Verify email.
Design: "calm control room" — Newsreader for headings, IBM Plex Sans for text, IBM Plex Mono for codes; slate-teal primary, amber "signal" colour only for live/attention states; light and dark themes.
Primary actions: Get support, Connect to my device, Manage plan. Make no unverified claims about competitors and never say "unlimited", "lifetime" or "undetectable".
```

### B2 — Accounts and security

```
Email + password accounts with roles user/admin. Email verification is required before starting sessions or buying (error EMAIL_UNVERIFIED). Password reset with single-use, hashed, 60-minute tokens that sign out all sessions; email on password change.
TOTP two-factor (RFC 6238) with the secret encrypted at rest (AES-GCM). Step-up re-authentication valid 10 minutes, required for enabling unattended access (or allowing control/audio unattended) and for starting each unattended session (codes STEP_UP_REQUIRED, MFA_SETUP_REQUIRED).
Audit log of security events (metadata only).
```

### B3 — Devices and host API

```
Tables: devices, device_public_keys, device_enrollment_codes, device_nonces, access_policies.
Owner creates an 8-digit enrollment code (15 min, single use, max 3 active hosts). Implement host/enroll, host/poll, host/invite, host/decide, host/session, host/banner_report exactly as in AGENTS.md, with nonce replay protection, ±60 s skew, 120 s lease renewed on every signed call, and DEVICE_REVOKED.
Device page: rename, enable unattended (needs MFA + step-up), revoke. Email the owner when a new device registers.
```

### B4 — Sessions, consent and permissions

```
Tables: invites, invite_attempts, remote_sessions, consent_events, banner_preferences.
Attended: viewer redeems a 12-digit support code (10 min, single use, 10 attempts/hour, generic error on failure) with requested permissions → session awaiting_approval. Unattended: owner requests a session to own device with unattended enabled.
Permissions { control, systemAudio, microphone, clipboard }, all off by default; host grant must be a subset. Viewer can end or request more permissions; only the host can approve.
State machine with compare-and-set transitions (see AGENTS.md); lazy fail-closed sweep with approval_timeout, consent_not_renewed (attended, default 30 min, up to 8 h by explicit approval), max_session_lifetime (8 h), host_lease_expired (120 s).
Consent events logged for every request, approval, renewal, escalation and end. Cross-account access returns 404.
Live updates to the browser through a per-user realtime channel.
Test host page (/dashboard/host-harness) that runs the host protocol in the browser with a non-extractable WebCrypto key in IndexedDB.
```

### B5 — Plans, billing and entitlements

```
Plans: one-time pass (24-hour window starting at the first approved session; redeem within 30 days; shown before payment), weekly, monthly, yearly. Prices live in a plan_config table editable by admins (seeded test prices: pass $4.99, weekly $3.99, monthly $9.99, yearly $89.00; relay hours 4/7/30/360).
Stripe via REST: hosted Checkout with price_data and tax_code txcd_10103000 (Managed Payments: no custom_text); reuse an open checkout instead of creating a duplicate; webhook verified on the raw body (STRIPE_WEBHOOK_SECRET) with event de-duplication; server-side reconciliation; customer portal; cancel at period end; emails for payment confirmed and renewal failed.
Only signed webhook events or reconciliation grant access — never a redirect.
Entitlement projection: subscription | pass_active | pass_ready | none. The session sponsor (the requester) needs an entitlement; a pass activates atomically at host approval; one live session per sponsor (402 otherwise).
Usage ledger with idempotent event ids: session seconds, direct seconds, relay seconds.
"Pay on phone": an opaque, expiring (15 min) QR link; the phone shows masked email, total and renewal schedule; the buyer must sign in as the same account; scanning alone grants nothing.
```

### B6 — Admin

```
Operations page for admins: totals, recent purchases and sessions, plan price editor, manual reconcile of a purchase. Admin-only routes return 403 for others.
```
