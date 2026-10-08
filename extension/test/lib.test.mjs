// Run with: node --test extension/test
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  SITE,
  pageUrl,
  parseDevicesResponse,
  nativeHostState,
  isValidNativeAnswer,
  deviceStatusLabel,
} from "../src/lib.js";

test("pages are on the website only", () => {
  for (const key of ["login", "devices", "support", "connect", "download"]) {
    const url = new URL(pageUrl(key));
    assert.equal(url.origin, new URL(SITE).origin, key);
  }
  assert.throws(() => pageUrl("nope"));
});

test("devices are read from a superjson answer and revoked ones are hidden", () => {
  const body = JSON.stringify({
    json: {
      devices: [
        { id: "a", name: "Office PC", platform: "windows", status: "active", online: true },
        { id: "b", name: "Old laptop", platform: "macos", status: "revoked", online: false },
        { id: "c", name: "Home", platform: "macos", status: "active", online: false },
      ],
    },
    meta: { values: { "devices.0.lastSeenAt": ["Date"] } },
  });
  const devices = parseDevicesResponse(body);
  assert.deepEqual(
    devices.map((d) => [d.name, d.online]),
    [["Office PC", true], ["Home", false]],
  );
});

test("an empty account and a missing list are told apart", () => {
  assert.deepEqual(parseDevicesResponse('{"json":{"devices":[]}}'), []);
  assert.throws(() => parseDevicesResponse('{"json":{}}'), /unexpected/);
  assert.throws(() => parseDevicesResponse("<html>"), /unreadable/);
});

test("device names are coerced and online is strict", () => {
  const [d] = parseDevicesResponse('{"json":{"devices":[{"id":1,"name":null,"online":"yes"}]}}');
  assert.equal(d.id, "1");
  assert.equal(d.name, "Unnamed device");
  assert.equal(d.online, false, "only a real true counts as online");
  assert.equal(deviceStatusLabel(d), "Offline");
});

test("the native host answer is accepted only in its two shapes", () => {
  assert.equal(isValidNativeAnswer({ installed: true, version: "0.1.0" }), true);
  assert.equal(isValidNativeAnswer({ installed: false }), true);
  assert.equal(isValidNativeAnswer({ installed: true }), false, "installed needs a version");
  assert.equal(isValidNativeAnswer({ installed: "yes" }), false);
  assert.equal(isValidNativeAnswer(null), false);
  assert.equal(isValidNativeAnswer("installed"), false);
});

test("the popup describes the native host state", () => {
  assert.deepEqual(nativeHostState({ installed: true, version: "0.1.0" }), {
    installed: true,
    version: "0.1.0",
    label: "Native host 0.1.0",
  });
  assert.equal(nativeHostState(null).installed, false, "no answer means not installed");
});
