// Pure helpers for the popup. No DOM, no chrome.* calls, so they are testable with node.

export const SITE = "https://remotebridge.floot.app/";
export const NATIVE_HOST = "com.remotebridge.host";

/** Pages on the website, opened in a new tab. */
export const PAGES = {
  login: "login",
  devices: "dashboard/devices",
  support: "dashboard/support",
  connect: "dashboard/sessions",
  download: "download",
};

export function pageUrl(key, base = SITE) {
  const path = PAGES[key];
  if (!path) throw new Error(`unknown page: ${key}`);
  return new URL(path, base).toString();
}

/**
 * Read the devices from a `devices/list` answer.
 * The website answers in superjson: `{"json": {...}, "meta": {...}}`. Only the
 * plain fields are needed here (names and the server-computed `online` flag),
 * so the `meta` dates are ignored. Revoked devices are hidden.
 */
export function parseDevicesResponse(text) {
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    throw new Error("unreadable answer from the website");
  }
  const devices = body?.json?.devices;
  if (!Array.isArray(devices)) {
    throw new Error("unexpected answer from the website");
  }
  return devices
    .filter((d) => d && d.status !== "revoked")
    .map((d) => ({
      id: String(d.id),
      name: String(d.name ?? "Unnamed device"),
      platform: String(d.platform ?? ""),
      online: d.online === true,
    }));
}

/** What the popup shows for the native host, from its status answer. */
export function nativeHostState(answer) {
  if (!answer || answer.installed !== true) {
    return { installed: false, label: "Native host not installed" };
  }
  const version = typeof answer.version === "string" ? answer.version : "unknown version";
  return { installed: true, version, label: `Native host ${version}` };
}

/** Accept only the two answers the native host is allowed to give. */
export function isValidNativeAnswer(value) {
  if (!value || typeof value !== "object") return false;
  if (typeof value.installed !== "boolean") return false;
  if (value.installed && typeof value.version !== "string") return false;
  return true;
}

/** Fields the popup may show about a device, with the status as a plain label. */
export function deviceStatusLabel(device) {
  return device.online ? "Online" : "Offline";
}
