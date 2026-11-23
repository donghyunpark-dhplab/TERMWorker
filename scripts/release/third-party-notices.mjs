#!/usr/bin/env node
// Writes THIRD-PARTY-NOTICES.md: every Rust crate and npm package that goes into
// the app (not dev-only tools), its licence, and the licence texts the packages
// ship (copyright lines included), each distinct text once.
//
//   node scripts/release/third-party-notices.mjs [output file]
//
// Needs `cargo` and an installed node_modules (npm ci).

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const out = path.resolve(process.argv[2] ?? path.join(root, "THIRD-PARTY-NOTICES.md"));

const LICENSE_FILE = /^(licen[cs]e|copying|notice|copyright|unlicense)([-._].*)?$/i;

/** Licence-like files in a package folder, top level only. */
function licenseTexts(dir) {
  let names = [];
  try {
    names = fs.readdirSync(dir).filter((n) => LICENSE_FILE.test(n) && fs.statSync(path.join(dir, n)).isFile());
  } catch {
    return [];
  }
  return names.sort().map((n) => ({ file: n, text: fs.readFileSync(path.join(dir, n), "utf8").replace(/\r\n/g, "\n").trim() }));
}

const texts = new Map(); // hash -> { id, text, file }
function textId(file, text) {
  const h = createHash("sha256").update(text).digest("hex");
  if (!texts.has(h)) texts.set(h, { id: `T${texts.size + 1}`, text, file });
  return texts.get(h).id;
}

// ---------------------------------------------------------------- Rust

function rustCrates() {
  const meta = JSON.parse(
    execFileSync("cargo", ["metadata", "--format-version", "1", "--locked", "--manifest-path", path.join(root, "src-tauri", "Cargo.toml")], {
      encoding: "utf8",
      maxBuffer: 256 << 20,
    }),
  );
  const byId = new Map(meta.packages.map((p) => [p.id, p]));
  const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]));
  // Normal and build dependencies of the app, on any platform; dev-dependencies are not shipped.
  const seen = new Set();
  const stack = [meta.resolve.root];
  while (stack.length) {
    const id = stack.pop();
    if (seen.has(id)) continue;
    seen.add(id);
    for (const d of nodes.get(id)?.deps ?? []) {
      if (d.dep_kinds.some((k) => k.kind !== "dev")) stack.push(d.pkg);
    }
  }
  seen.delete(meta.resolve.root);
  return [...seen]
    .map((id) => byId.get(id))
    .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version))
    .map((p) => ({
      name: p.name,
      version: p.version,
      license: p.license ?? (p.license_file ? `see ${p.license_file}` : "UNKNOWN"),
      url: p.repository ?? p.homepage ?? `https://crates.io/crates/${p.name}`,
      texts: licenseTexts(path.dirname(p.manifest_path)),
    }));
}

// ---------------------------------------------------------------- npm

function npmPackages() {
  const lock = JSON.parse(fs.readFileSync(path.join(root, "package-lock.json"), "utf8"));
  return Object.entries(lock.packages)
    .filter(([key, p]) => key.startsWith("node_modules/") && !p.dev && !p.optional)
    .map(([key, p]) => {
      const dir = path.join(root, key);
      let pkg = {};
      try {
        pkg = JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8"));
      } catch {
        // not installed
      }
      const name = key.slice(key.lastIndexOf("node_modules/") + "node_modules/".length);
      const repo = typeof pkg.repository === "string" ? pkg.repository : pkg.repository?.url;
      return {
        name,
        version: p.version,
        license: p.license ?? pkg.license ?? "UNKNOWN",
        url: (repo ?? pkg.homepage ?? `https://www.npmjs.com/package/${name}`).replace(/^git\+/, ""),
        texts: licenseTexts(dir),
      };
    })
    .sort((a, b) => a.name.localeCompare(b.name));
}

// ---------------------------------------------------------------- output

function section(title, list) {
  const lines = [`## ${title}`, ""];
  const counts = new Map();
  for (const p of list) counts.set(p.license, (counts.get(p.license) ?? 0) + 1);
  lines.push(`${list.length} packages. Licences: ${[...counts].sort((a, b) => b[1] - a[1]).map(([l, n]) => `${l} (${n})`).join(", ")}.`, "");
  lines.push("| Package | Version | Licence | Source | Licence text |", "|---|---|---|---|---|");
  for (const p of list) {
    const ids = p.texts.map((t) => textId(t.file, t.text));
    const spdx = p.license.split(/\s+(?:OR|AND|WITH)\s+|[/()]/).map((s) => s.trim()).filter(Boolean)[0];
    const ref = ids.length ? ids.join(", ") : `none shipped; standard text: https://spdx.org/licenses/${spdx}.html`;
    lines.push(`| ${p.name} | ${p.version} | ${p.license} | ${p.url} | ${ref} |`);
  }
  lines.push("");
  return lines;
}

const rust = rustCrates();
const npm = npmPackages();
const missing = [...rust, ...npm].filter((p) => p.license === "UNKNOWN");
const doc = [
  "# Third-party notices",
  "",
  "TermWorker includes the following third-party software. Each package is used under the licence",
  "shown; where a package offers a choice (for example \"MIT OR Apache-2.0\"), TermWorker uses it",
  "under the permissive option. The licence texts shipped with the packages follow, each distinct text",
  "once (T1, T2, …). For the few packages that ship no licence file, the table links the standard",
  "text of their licence.",
  "",
  "This list is generated by `scripts/release/third-party-notices.mjs` from Cargo.lock and",
  "package-lock.json; build tools and test-only packages are not listed because they are not shipped.",
  "",
  ...section("Rust crates (backend)", rust),
  ...section("npm packages (user interface)", npm),
  "## Linux AppImage: bundled system libraries",
  "",
  "The AppImage additionally contains system libraries from Ubuntu 22.04 that TermWorker links",
  "dynamically: WebKitGTK and JavaScriptCore, GTK 3, GLib, libsoup, Pango, Cairo, HarfBuzz, GStreamer",
  "and others. Most are licensed under the GNU LGPL 2.1 or later (some under the LGPL 2.0, MPL or",
  "MIT-style licences). They are unmodified Ubuntu packages, bundled as separate shared libraries that",
  "can be replaced. The list of bundled libraries with their Ubuntu package names and versions, the",
  "copyright files of those packages, and the full LGPL text ship inside the AppImage; the",
  "corresponding source code is available from the Ubuntu archive (https://packages.ubuntu.com,",
  "source packages of the jammy release) or on request from DHPLab Consulting for three years after",
  "each release. The .deb package and the Windows installer bundle none of these libraries.",
  "",
  "## Licence texts",
  "",
  ...[...texts.values()].flatMap((t) => [`### ${t.id} (${t.file})`, "", "```text", t.text, "```", ""]),
];
fs.writeFileSync(out, doc.join("\n"));
console.log(`${path.relative(process.cwd(), out)}: ${rust.length} crates, ${npm.length} npm packages, ${texts.size} distinct licence texts`);
if (missing.length) {
  console.error(`No licence for: ${missing.map((p) => `${p.name}@${p.version}`).join(", ")}`);
  process.exitCode = 1;
}
