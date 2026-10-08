# RemoteBridge extension: privacy practices

Last updated: 8 October 2026

## What the extension does

The RemoteBridge extension is an optional companion for the RemoteBridge website. It shows your devices and their online status, and opens the website pages for support and connection.

## What it does not do

- It does not read the content of any web page. It has no content scripts and no access to other sites.
- It does not read browsing history, tabs or cookies, and it has no access to anything except the RemoteBridge website.
- It does not record, transmit or store screen content, keystrokes, clipboard text or audio.
- It does not include analytics, advertising or tracking code.
- It does not load or run any remote code.

## Data it handles

| Data | Where it goes | Why | Kept |
|---|---|---|---|
| Your RemoteBridge session (cookie set by the website) | Sent only to remotebridge.floot.app, by the browser, with the device list request | To show your devices | Not read or stored by the extension |
| Device names, platform and online status | Received from remotebridge.floot.app | To show your devices | Cached locally in the browser (chrome.storage.local) so the list can be shown offline. Cleared when you sign out |
| Native host status (installed or not, version) | Stays on your computer, between the browser and the optional host app | To show whether the host app is installed | Not stored |

## Permissions and why

- **storage**: keeps the last device list so the popup can show it when you are offline. Cleared when the website reports that you are signed out.
- **nativeMessaging**: talks to the optional host app on your computer. The host app answers only two questions: whether it is installed (and its version), and a request to open its window. Nothing else is exchanged.
- **Access to remotebridge.floot.app only**: needed to fetch your device list. The extension has no access to any other site.

## Your choices

- Signing out of the website removes the cached device list the next time the popup opens.
- Removing the extension deletes everything it stored.
- The native host is optional. Without it, the popup works and offers a link to the download page.

## Contact

Questions about this policy: use the Help page on the RemoteBridge website.
