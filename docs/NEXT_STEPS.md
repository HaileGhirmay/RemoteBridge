# RemoteBridge: what is next, step by step

Written after the Windows host, the release build, the signaling server and the
Chrome extension were all tested live (October 2026). Each section is one job.
Do them in the order given unless a section says it can wait.

## Where things stand

| Part | State |
|---|---|
| Website and control plane (Floot) | Live at `https://remotebridge.floot.app` |
| Windows host (`rb-host.exe`) | Works: enroll, approve, video, audio, control, clipboard, hide/restore, share code, unattended toggle, autostart, single instance |
| Release build | `.github/workflows/release.yml` makes `remotebridge-host-windows.zip` on every tag or manual run |
| Signaling server | Live on Fly.io at `wss://remotebridge-signaling.fly.dev/ws` |
| TURN relay | **Not deployed.** Sessions work only when a direct connection is possible |
| Chrome extension | Works unpacked in Chrome. **Not in any store yet** |
| Code signing | **Not set up.** Windows shows a SmartScreen warning on the unsigned exe |
| Installer and auto-update | **Not built.** Install is "unzip into `%LOCALAPPDATA%\RemoteBridge`" |
| macOS host | **Not started.** Needs a Mac |

---

## 1. Signaling: keep it healthy (15 minutes, then ongoing)

Already deployed. These are the checks and the two things you will need to do at some point.

### 1a. Health check

```
curl https://remotebridge-signaling.fly.dev/healthz
```

Expect `ok`. If not:

```
fly status --app remotebridge-signaling
fly logs --app remotebridge-signaling
```

### 1b. See what is running

```
fly machines list --app remotebridge-signaling
```

Two machines in `fra` is the current setup. Sessions are long-lived WebSockets, so
`auto_stop_machines` stays `off` in `signaling/fly.toml`. Do not change that.

### 1c. Redeploy after a code change

From the **repository root** (the Docker build needs the whole workspace):

```
fly deploy --config signaling/fly.toml --dockerfile signaling/Dockerfile
```

Then run the health check again.

### 1d. Rotate the signaling secret

Do this if the secret was ever shown on screen or shared. Both sides must get the
**same** new value, and sessions in flight will drop once, so do it at a quiet time.

1. Make a new value and keep it somewhere safe, never in chat or git:
   ```
   openssl rand -base64 48
   ```
2. Set it on Fly:
   ```
   fly secrets set --app remotebridge-signaling SIGNALING_JWT_SECRET="<new value>"
   ```
3. Set the same value on the website: Floot project, Settings, Secrets, `SIGNALING_JWT_SECRET`.
4. Start a session from the website and confirm the viewer connects.

---

## 2. TURN relay (half a day, needs a small VM)

Without TURN, two machines behind strict NATs or corporate firewalls cannot connect.
The website already hands out TURN credentials when `TURN_URLS` and
`TURN_SHARED_SECRET` are set, and the host and viewer already use them.

Full details are in `signaling/DEPLOY.md` and `signaling/turn/FIREWALL.md`. The short form:

1. Rent a VM with a **public IPv4** address (1 vCPU, 1 to 2 GB RAM is enough). Fly cannot host TURN because it needs a UDP port range.
2. Point a DNS name at it, for example `turn.remotebridge.example` (use your own domain).
3. Open these inbound ports on the VM firewall and the cloud security group: UDP 3478, TCP 3478, TCP 5349, TCP 443, UDP 49152 to 65535.
4. Install Docker on the VM. Get a TLS certificate for the DNS name with certbot and copy `fullchain.pem` and `privkey.pem` into `signaling/turn/tls/`.
5. Make the shared secret once and keep it safe:
   ```
   openssl rand -base64 48
   ```
6. If the VM is behind cloud NAT (AWS, GCP, Azure, Hetzner cloud all are), edit `signaling/turn/turnserver.conf` and set `external-ip=<public ip>/<private ip>`.
7. Start coturn:
   ```
   cd signaling/turn
   TURN_SHARED_SECRET="<value from step 5>" docker compose up -d coturn
   ```
8. Test it from your laptop:
   ```
   TURN_SHARED_SECRET="<value>" signaling/turn/test-credentials.sh turn.remotebridge.example
   ```
   All three cases must pass: valid accepted, tampered refused, expired refused.
9. On the website (Floot, Settings, Secrets) set:
   - `TURN_URLS` = `turn:turn.remotebridge.example:3478?transport=udp,turns:turn.remotebridge.example:443?transport=tcp`
   - `TURN_SHARED_SECRET` = the value from step 5
10. Prove it works: on the viewer machine, block UDP in Windows Firewall or use a phone hotspot with UDP blocked, start a session, and check the session page shows **relay** instead of **direct**.
11. Check the clock on the VM is NTP-synced (`timedatectl`). TURN credentials expire by timestamp; a skewed clock rejects good ones.

---

## 3. Code signing for the Windows exe (1 to 3 days, mostly waiting on the vendor)

Unsigned, `rb-host.exe` triggers SmartScreen "Windows protected your PC" on every
new machine. The release workflow already signs when two GitHub secrets exist.

### 3a. Get a certificate

Pick one:

- **Azure Trusted Signing** (cheapest, about 10 USD a month, no hardware token). Needs an Azure account and identity validation of you or your company. The workflow would need a small change to use the Azure signing action instead of a PFX file. Ask for that change when you choose this route.
- **OV code-signing certificate** from a vendor such as SSL.com, Sectigo or DigiCert (about 200 to 400 USD a year). Since 2023 these ship on a hardware token or in a cloud HSM, so you cannot export a plain PFX. Vendors offer a cloud signing service that works with `signtool`. Again, the workflow needs a small change for that vendor's tool.
- **Self-signed, for internal testing only.** Works with the workflow as it is today, but SmartScreen still warns. Only useful to prove the signing step runs.

A self-signed certificate for the test route, in PowerShell as your user:

```powershell
$cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=RemoteBridge Test" -CertStoreLocation Cert:\CurrentUser\My
$pw = Read-Host -AsSecureString "PFX password"
Export-PfxCertificate -Cert $cert -FilePath "$env:USERPROFILE\rb-test.pfx" -Password $pw
[Convert]::ToBase64String([IO.File]::ReadAllBytes("$env:USERPROFILE\rb-test.pfx")) | Set-Clipboard
```

The clipboard now holds the base64 text for the secret.

### 3b. Add the secrets on GitHub

Repository, Settings, Secrets and variables, Actions, New repository secret:

| Name | Value |
|---|---|
| `CODESIGN_PFX_BASE64` | the base64 text of the `.pfx` file |
| `CODESIGN_PFX_PASSWORD` | the PFX password |

Never paste either value anywhere else.

### 3c. Run the release and check the signature

1. GitHub, Actions, Release, Run workflow, branch `main`.
2. Download the artifact, unzip, then in PowerShell:
   ```powershell
   Get-AuthenticodeSignature .\rb-host.exe | Format-List Status, SignerCertificate
   ```
   `Status` must be `Valid` (or `UnknownError` for a self-signed test certificate, which is expected).
3. Right-click the exe, Properties, Digital Signatures tab shows the signer.

### 3d. Make a real release

```
git tag v0.1.0
git push origin v0.1.0
```

The workflow builds, signs and attaches the zip to a GitHub release at
`https://github.com/HaileGhirmay/RemoteBridge/releases`. Bump the workspace
version in the root `Cargo.toml` before each new tag.

---

## 4. Chrome Web Store and Edge Add-ons (2 to 3 days, mostly review wait)

### 4a. Accounts

- Chrome: `https://chrome.google.com/webstore/devconsole`, one-time 5 USD registration.
- Edge: `https://partner.microsoft.com/dashboard/microsoftedge`, free.

### 4b. Build the package

From the repository root on Windows:

```powershell
powershell -ExecutionPolicy Bypass -File tools\package-extension.ps1
```

This runs the tests and the lint, then writes `dist\remotebridge-extension-0.1.0.zip`.

### 4c. Submit

1. Upload the zip.
2. Copy the listing text from `extension/store/LISTING.md` and the privacy statement from `extension/store/PRIVACY.md` into the forms.
3. Permissions justification (the form asks for each):
   - `storage`: remembers the popup's last device list so it opens instantly.
   - `nativeMessaging`: talks to the optional `rb-native-host.exe`, which only answers "is the host installed" and "open the host window".
   - Host access to `https://remotebridge.floot.app/*`: reads the signed-in user's own device list from the website.
4. Screenshots: the popup signed in with devices (1280x800 or 640x400), and the popup with the native host detected.
5. Submit for review. Chrome takes a few days, Edge about the same.

### 4d. After publishing

The store assigns a **new extension id**. Then:

1. Put that id in the native host install step. The release zip's `README.txt` and `extension/README.md` tell users to run `tools\install-native-host.ps1` with the id. Update both to show the real id so users can copy it.
2. Install from the store on a clean browser profile and repeat the popup test: signed in, devices listed, native host found, Open starts the host.

---

## 5. Installer and auto-update (decision needed first)

Today the install is "unzip into `%LOCALAPPDATA%\RemoteBridge`, run `rb-host.exe autostart on`". That is fine for testers, not for customers.

Decide:

1. **Installer format.** Options: a per-user MSI built with WiX, or an Inno Setup exe. Both can register autostart, write the native-host manifest, and uninstall cleanly. Inno Setup is quicker to get right.
2. **Auto-update.** The host already reads `updateRequired` and `minProtocolVersion` from `host/poll` and refuses to run when too old. What is missing is fetching and applying the new version. Options: the host checks the GitHub releases feed and downloads the signed zip, or a small updater exe. Do not build this until code signing (section 3) is done, because an updater that installs unsigned binaries is a security hole.

Tell me which you pick and I will build it. Rough size: installer one day, updater two to three days plus tests.

---

## 6. macOS host (needs a Mac, several weeks)

The shared core, protocol client, session manager, timers and permission filtering are all platform-independent and tested on macOS in CI already. What is missing is the macOS equivalent of `platform/windows`:

1. Screen capture with ScreenCaptureKit, system audio with the same framework.
2. Input injection with CGEvent, which needs the Accessibility permission.
3. Key store in the Secure Enclave or Keychain, with DER to P1363 signature conversion.
4. Menu bar item, banner window, consent dialogs, global shortcuts `Control+Option+Shift+B` and `X`.
5. Signed and notarized app bundle, Launch Agent for autostart.

Before starting you need: a Mac on macOS 14 or later, Xcode, and an Apple Developer account (99 USD a year) for signing and notarization. The Swift package in `platform/macos/` is the starting point.

---

## 7. Two-machine acceptance run (2 hours, do this before every release)

Use two Windows PCs on different networks (one on a phone hotspot is enough). PC A runs the host, PC B runs the browser.

| # | Step | Expect |
|---|---|---|
| 1 | On A: unzip the release, run `rb-host.exe enroll <code>`, then `autostart on`, sign out and in | Tray icon, no console window |
| 2 | On A: tray, Share this computer | 12-digit code |
| 3 | On B: Support page, enter the code, request view only | A shows the approval window naming B's account |
| 4 | On A: Allow | B shows the live screen within 5 s |
| 5 | On B: request control | A shows a new approval with only Control ticked; B cannot control until A allows |
| 6 | On A: `Ctrl+Alt+Shift+B` | Banner and tray hide, then come back on the shortcut again |
| 7 | On B: Enable sound while A plays audio | Audio on B |
| 8 | Wait for the consent renewal prompt (30 min, or set a shorter policy on the website) | A is asked to renew; declining ends the session |
| 9 | On A: `Ctrl+Alt+Shift+X` | Session ends at once; the website shows `emergency_disconnect` |
| 10 | On A: tray, Quit | Website shows `host_shutdown`; the tray icon is gone |
| 11 | On the website: Remove device A | A's host wipes its key and shows "not enrolled" on next start |

Write the result of each row in the release notes.

---

## 8. Small things noted during testing

Not blocking, do them when convenient. Each is a short task.

- `rb-host.exe` has no Windows version resource, so Task Manager and Properties show no version or company name. Add a `.rc` resource at build time.
- Expired one-time passes stay marked "ready" on the Plan and billing page until the next request. A nightly sweep on the website would tidy them up.
- The "already running" message prints three times when a second `rb-host.exe run` is started. Trim to one line.
- The approval window has a faint grey band behind the intro text on some themes.
- Decide whether switching the tray's unattended option off should end a live unattended session immediately (current behaviour: it stops new ones only).
