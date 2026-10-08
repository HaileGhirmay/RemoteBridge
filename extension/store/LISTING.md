# Store listing (Chrome Web Store and Edge Add-ons)

## Name
RemoteBridge

## Short description (132 characters maximum)
See your RemoteBridge devices, get support, and open the host app.

## Full description
RemoteBridge is consent-based remote desktop. This optional companion shows your RemoteBridge devices and whether they are online, and opens the website pages to get support or connect.

What it does:
- Signs you in through the RemoteBridge website. The extension never asks for your password.
- Lists your devices and their online status.
- Opens "Get support" and "Connect to my device" on the website.
- If the RemoteBridge host app is installed on your computer, it shows that and can open its window.

What it does not do:
- It does not read the pages you visit.
- It does not control your computer or record anything.
- It works without the host app. The host app is a separate download.

Access is consent-based: a person at the host approves every session, and remote control, system audio, microphone and clipboard are each separate permissions that start off.

## Category
Productivity

## Language
English

## Single purpose
Show the status of the user's RemoteBridge devices and open RemoteBridge website pages.

## Permission justifications (for the review form)
- **storage**: caches the last device list so it can be shown offline; cleared on sign-out.
- **nativeMessaging**: optional link to the RemoteBridge host app on the user's computer; it answers only "status" and "open".
- **Host access to https://remotebridge.floot.app/***: fetches the user's device list with the user's website session. No other host is accessed.

## Remote code
None. All code is in the package. No eval, no remote scripts.

## Data use (for the privacy form)
- Collected by the extension: none. The extension stores device names and online status locally, to show offline, and sends nothing to a third party.
- Sold to third parties: no. Used for advertising: no. Used for anything unrelated to the single purpose: no.
- Privacy policy: see PRIVACY.md.

## Screenshots
Popup with devices, popup signed out, popup with the host app installed. Capture these in Chrome before submitting; they are not generated here.
