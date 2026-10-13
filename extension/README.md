# RemoteBridge companion extension (Chrome and Edge, MV3)

Optional. Shows your RemoteBridge devices and whether they are online, opens
the website pages for getting support or connecting, and, when the host app is
installed on this computer, says so and can start it. It reads no web pages and
never touches a session's screen, input or audio.

| File | What |
|---|---|
| `manifest.json` | MV3; permissions `storage` and `nativeMessaging`; host access to the website only. |
| `popup.html`, `popup.css`, `src/popup.js` | The toolbar popup. |
| `src/lib.js` | Pure helpers (URL building, parsing the device list, native-host answers); unit-tested. |
| `test/lib.test.mjs` | `node --test` tests for `lib.js`. |
| `store/LISTING.md`, `store/PRIVACY.md` | Store listing text and privacy statement. |

## Try it unpacked

1. Open `chrome://extensions` (or `edge://extensions`), switch on **Developer mode**, click **Load unpacked** and pick this `extension/` folder.
2. Note the 32-letter extension id shown on the card.
3. Sign in on the website in a normal tab; the popup uses that session.

## The native host link (optional)

The popup can talk to `rb-native-host.exe`, a tiny shim that answers only
`{"type":"status"}` and `{"type":"open"}`. It looks for `rb-host.exe` and
`rb-host.version` in `%LOCALAPPDATA%\RemoteBridge` (the release zip ships both;
the host also rewrites the version file on every start). Register it once for
your browser profile, with the id from step 2:

```powershell
powershell -File tools\install-native-host.ps1 -HostPath "$env:LOCALAPPDATA\RemoteBridge\rb-native-host.exe" -ExtensionId <id>
```

**Open** runs `rb-host.exe --open-window`, which starts the host in the tray if
it is not already running and does nothing otherwise.

## Checks and packaging

```powershell
node --test extension/test/lib.test.mjs
node tools/extension-lint.mjs extension
powershell -File tools/package-extension.ps1     # runs both, then writes dist/remotebridge-extension-<version>.zip
```

Submitting the zip to the Chrome Web Store and Edge Add-ons needs the developer
accounts; the texts under `store/` are what the forms ask for.
