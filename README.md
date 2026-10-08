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
