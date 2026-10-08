import {
  SITE,
  NATIVE_HOST,
  pageUrl,
  parseDevicesResponse,
  nativeHostState,
  isValidNativeAnswer,
  deviceStatusLabel,
} from "./lib.js";

const $ = (id) => document.getElementById(id);

function open(key) {
  chrome.tabs.create({ url: pageUrl(key) });
}

function setText(id, text) {
  $(id).textContent = text;
}

function showSignedOut() {
  setText("account", "Not signed in");
  $("signin").hidden = false;
  $("devices").replaceChildren();
}

const CACHE_KEY = "devicesCache";

/** The last device list we got, so the popup can show it while offline. */
async function readCache() {
  try {
    const stored = await chrome.storage.local.get(CACHE_KEY);
    return stored[CACHE_KEY] ?? null;
  } catch {
    return null;
  }
}

async function writeCache(devices) {
  try {
    await chrome.storage.local.set({ [CACHE_KEY]: { at: Date.now(), devices } });
  } catch {
    // Storage is a convenience; the popup works without it.
  }
}

function renderDevices(devices, { cached } = {}) {
  const list = $("devices");
  list.replaceChildren(
    ...devices.map((d) => {
      const item = document.createElement("li");
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = d.name;
      const status = document.createElement("span");
      status.className = d.online && !cached ? "status live" : "status";
      status.textContent = cached ? "Unknown" : deviceStatusLabel(d);
      item.append(name, status);
      return item;
    }),
  );
}

async function loadDevices() {
  let response;
  try {
    // The website's own session cookie. The extension has host permission for
    // the website only; nothing else is read.
    response = await fetch(new URL("_api/devices/list", SITE), {
      method: "GET",
      credentials: "include",
      headers: { accept: "application/json" },
    });
  } catch {
    const cache = await readCache();
    if (cache) {
      const when = new Date(cache.at).toLocaleString();
      setText("account", `Offline. Showing devices from ${when}`);
      renderDevices(cache.devices, { cached: true });
      $("signin").hidden = true;
    } else {
      setText("account", "Can't reach remotebridge.floot.app");
    }
    return;
  }
  if (response.status === 401) {
    // Signed out: forget the cached list rather than show someone else's devices.
    try {
      await chrome.storage.local.remove(CACHE_KEY);
    } catch {
      // nothing to forget
    }
    showSignedOut();
    return;
  }
  if (!response.ok) {
    setText("account", `Website error ${response.status}`);
    return;
  }
  let devices;
  try {
    devices = parseDevicesResponse(await response.text());
  } catch (err) {
    setText("account", err.message);
    return;
  }
  setText("account", devices.length ? "Signed in" : "Signed in, no devices yet");
  $("signin").hidden = true;
  renderDevices(devices);
  await writeCache(devices);
}

function askNative(message) {
  return new Promise((resolve) => {
    try {
      chrome.runtime.sendNativeMessage(NATIVE_HOST, message, (answer) => {
        // Without the native host, Chrome reports an error here. That is the
        // normal "not installed" case, not a failure of the extension.
        if (chrome.runtime.lastError) {
          resolve(null);
          return;
        }
        resolve(isValidNativeAnswer(answer) ? answer : null);
      });
    } catch {
      resolve(null);
    }
  });
}

async function loadNativeHost() {
  const answer = await askNative({ type: "status" });
  const state = nativeHostState(answer);
  setText("native", state.label);
  $("open-host").hidden = !state.installed;
  $("download-host").hidden = state.installed;
}

function wire() {
  $("signin-btn").addEventListener("click", () => open("login"));
  $("support").addEventListener("click", () => open("support"));
  $("connect").addEventListener("click", () => open("connect"));
  $("devices-link").addEventListener("click", () => open("devices"));
  $("download-host").addEventListener("click", () => open("download"));
  $("open-host").addEventListener("click", async () => {
    // Asks the native host to show its window. It answers only {"ok": true}.
    await askNative({ type: "open" });
  });
}

wire();
loadDevices();
loadNativeHost();
