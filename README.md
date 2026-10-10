# RemoteBridge host

Native host for RemoteBridge. `AGENTS.md` has the rules and protocol; `PROMPTS.md` has the build order.

| Path | What |
|---|---|
| `core/` | Rust library: protocol client, session manager, consent and banner timers, permission filter. No platform code. |
| `platform/windows/` | Windows 11 adapters (Rust). |
| `platform/macos/` | macOS 13+ Swift package; `bridge/` is the Rust staticlib with a C ABI. |
| `signaling/`, `viewer/`, `extension/` | Later prompts. |
| `tools/` | `check.ps1` / `check.sh` run the same fmt, clippy and test steps as CI. |

Run the checks with `tools/check.ps1` (Windows) or `tools/check.sh` (macOS).

## Windows host: install and run

Download `remotebridge-host-windows-<version>.zip` from the GitHub release (or build it with `cargo build --release -p rb-host-app`; libopus needs CMake). Then, in PowerShell from the unzipped folder:

| Step | Command |
|---|---|
| Register this PC once (8-digit code from My devices on the website) | `.\rb-host.exe enroll <code>` |
| Run the host (lives in the tray; right-click for the menu) | `.\rb-host.exe run` |
| Start at login (needed for unattended access) | `.\rb-host.exe autostart on` |
| Support code without the tray | `.\rb-host.exe invite` |

The tray menu has **Share this computer…** (the 12-digit code), **Show indicators**, **Disconnect now**, the **unattended access** switch (its other half is the owner's toggle on the website) and **Quit**. Without a terminal, logs go to `%LOCALAPPDATA%\RemoteBridge\logs\rb-host.log`; they are metadata only.

Releases are built by `.github/workflows/release.yml` on a `v*` tag. The exe is signed when the repository has the `CODESIGN_PFX_BASE64` and `CODESIGN_PFX_PASSWORD` secrets; until then SmartScreen warns on first run.
