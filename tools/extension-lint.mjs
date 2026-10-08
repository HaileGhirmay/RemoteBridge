// Local checks for the Chrome/Edge extension, modelled on the Chrome Web Store review.
// Run with: node tools/extension-lint.mjs [extension-dir]
// Exit code 1 if anything is wrong. Warnings do not fail the run.
import { readFileSync, existsSync, readdirSync, statSync } from "node:fs";
import { join, relative, resolve } from "node:path";

const dir = resolve(process.argv[2] ?? "extension");
const errors = [];
const warnings = [];

// Permissions we accept, and why. Anything else needs a justification here first.
const ALLOWED_PERMISSIONS = new Map([
  ["storage", "remember the popup's last state"],
  ["nativeMessaging", "talk to the optional native host (status and open only)"],
]);
const SITE_PATTERN = "https://remotebridge.floot.app/*";
const BROAD = /<all_urls>|\*:\/\/\*|https?:\/\/\*\/|^\*$|file:\/\//;

function read(path) {
  return readFileSync(join(dir, path), "utf8");
}

function listFiles(root, base = root) {
  const out = [];
  for (const name of readdirSync(root)) {
    const full = join(root, name);
    if (statSync(full).isDirectory()) {
      if (name === "test" || name === "node_modules") continue;
      out.push(...listFiles(full, base));
    } else {
      out.push(relative(base, full).replaceAll("\\", "/"));
    }
  }
  return out;
}

// ---- manifest -----------------------------------------------------------------
let manifest;
try {
  manifest = JSON.parse(read("manifest.json"));
} catch (e) {
  console.error(`manifest.json: ${e.message}`);
  process.exit(1);
}

if (manifest.manifest_version !== 3) errors.push("manifest_version must be 3");
if (!/^\d+\.\d+\.\d+$/.test(manifest.version ?? "")) errors.push("version must be x.y.z");
if (!manifest.name || manifest.name.length > 45) errors.push("name is required (max 45 characters)");
if (!manifest.description || manifest.description.length > 132) {
  errors.push("description is required (max 132 characters)");
}

for (const p of manifest.permissions ?? []) {
  if (!ALLOWED_PERMISSIONS.has(p)) {
    errors.push(`permission "${p}" is not on the allow-list (tools/extension-lint.mjs)`);
  }
}
for (const p of manifest.optional_permissions ?? []) {
  errors.push(`optional permission "${p}" is not allowed in this extension`);
}
if (manifest.permissions?.includes("nativeMessaging")) {
  warnings.push("nativeMessaging is needed for the optional host; keep it, but say so in the listing");
}

const hosts = manifest.host_permissions ?? [];
if (JSON.stringify(hosts) !== JSON.stringify([SITE_PATTERN])) {
  errors.push(`host_permissions must be exactly ["${SITE_PATTERN}"]`);
}
for (const h of hosts) {
  if (BROAD.test(h)) errors.push(`host permission "${h}" is too broad`);
}
for (const forbidden of ["content_scripts", "web_accessible_resources", "externally_connectable", "optional_host_permissions", "declarative_net_request"]) {
  if (manifest[forbidden] !== undefined) errors.push(`"${forbidden}" is not allowed in this extension`);
}

const csp = manifest.content_security_policy?.extension_pages;
if (csp && /unsafe-eval|unsafe-inline|https?:/.test(csp)) {
  errors.push("content_security_policy must not allow eval, inline code or remote scripts");
}

// ---- files referenced by the manifest exist --------------------------------
const referenced = [manifest.action?.default_popup, ...Object.values(manifest.icons ?? {}), ...Object.values(manifest.action?.default_icon ?? {})];
for (const file of referenced.filter(Boolean)) {
  if (!existsSync(join(dir, file))) errors.push(`missing file referenced by the manifest: ${file}`);
}

// Icons must be PNGs of the size their key says.
for (const [size, file] of Object.entries(manifest.icons ?? {})) {
  const bytes = existsSync(join(dir, file)) ? readFileSync(join(dir, file)) : null;
  if (!bytes) continue;
  const isPng = bytes.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]));
  const width = bytes.readUInt32BE(16);
  const height = bytes.readUInt32BE(20);
  if (!isPng || width !== Number(size) || height !== Number(size)) {
    errors.push(`${file} must be a ${size}x${size} PNG`);
  }
}

// ---- HTML and JavaScript ----------------------------------------------------------
const files = listFiles(dir);
const sitePrefix = SITE_PATTERN.replace("/*", "");
for (const file of files) {
  if (file.endsWith(".html")) {
    const html = read(file);
    for (const m of html.matchAll(/<script\b[^>]*\bsrc="([^"]+)"/g)) {
      if (/^https?:/.test(m[1])) errors.push(`${file}: remote script ${m[1]}`);
    }
    if (/<script\b(?![^>]*\bsrc=)[^>]*>/.test(html)) errors.push(`${file}: inline script is not allowed`);
    if (/\son[a-z]+="/i.test(html)) errors.push(`${file}: inline event handler is not allowed`);
  }
  if (file.endsWith(".js") || file.endsWith(".mjs")) {
    const js = read(file);
    if (/\beval\s*\(|new Function\s*\(/.test(js)) errors.push(`${file}: eval is not allowed`);
    if (/importScripts\s*\(/.test(js)) errors.push(`${file}: importScripts is not allowed`);
    if (/\.innerHTML\s*=/.test(js)) errors.push(`${file}: innerHTML is not used; build text with textContent`);
    for (const m of js.matchAll(/["'`](https?:\/\/[^"'`\s]+)["'`]/g)) {
      if (!m[1].startsWith(sitePrefix) && !m[1].startsWith("https://www.w3.org/")) {
        errors.push(`${file}: network address outside the website: ${m[1]}`);
      }
    }
  }
}

// Only what the store should see: no tests, no source maps, no stray files.
for (const file of files) {
  if (/\.(map|log|bak|tmp)$/.test(file) || file.startsWith(".")) {
    warnings.push(`unexpected file in the package: ${file}`);
  }
}

// ---- report -----------------------------------------------------------------------------
for (const w of warnings) console.warn(`warning: ${w}`);
for (const e of errors) console.error(`error: ${e}`);
if (errors.length === 0) {
  console.log(`extension lint passed (${files.length} files, ${warnings.length} warnings)`);
  console.log(`permissions: ${(manifest.permissions ?? []).map((p) => `${p} (${ALLOWED_PERMISSIONS.get(p)})`).join("; ")}`);
  console.log(`host access: ${hosts.join(", ")}`);
} else {
  process.exit(1);
}
