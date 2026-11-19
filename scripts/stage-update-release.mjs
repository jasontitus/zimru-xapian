#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Release-only configuration and immutable staging; no generated trust roots.
import { readFile, readdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { generate, verifyReleaseArtifact, validateVersion, validateBaseUrl } from "./make-update-manifest.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const targets = ["darwin-aarch64-app", "darwin-x86_64-app", "windows-x86_64-nsis", "linux-x86_64-appimage"];

function required(name) {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`Release requires ${name}`);
  return value;
}

export function releaseSettings(env = process.env) {
  const version = validateVersion(env.KIWIX_RELEASE_VERSION);
  const repository = env.GITHUB_REPOSITORY;
  const [owner, name, extra] = (repository ?? "").split("/");
  if (!/^[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?$/.test(owner ?? "") ||
      !/^[A-Za-z0-9_.-]{1,100}$/.test(name ?? "") ||
      name === "." || name === ".." || extra !== undefined) {
    throw new Error("Release requires a valid owner/repository in GITHUB_REPOSITORY");
  }
  const endpoint = `https://github.com/${repository}/releases/latest/download/update.json`;
  const publicKey = env.KIWIX_UPDATER_PUBLIC_KEY?.trim();
  const keyRecord = Buffer.from(Buffer.from(publicKey ?? "", "base64").toString("utf8").trim().split(/\r?\n/)[1] ?? "", "base64");
  if (keyRecord.length !== 42 || keyRecord.subarray(0, 2).toString() !== "Ed") throw new Error("Release requires a real base64 Tauri minisign public key in KIWIX_UPDATER_PUBLIC_KEY");
  return { version, publicKey, endpoint, baseUrl: validateBaseUrl(`https://github.com/${repository}/releases/download/v${version}/`).href };
}

async function main() {
  const settings = releaseSettings();
  const config = JSON.parse(await readFile(path.join(root, "src-tauri/tauri.conf.json"), "utf8"));
  const pkg = JSON.parse(await readFile(path.join(root, "package.json"), "utf8"));
  const cargo = await readFile(path.join(root, "src-tauri/Cargo.toml"), "utf8");
  const cargoVersion = cargo.match(/^\[package\][\s\S]*?^version\s*=\s*"([^"]+)"/m)?.[1];
  if ([config.version, pkg.version, cargoVersion].some((version) => version !== settings.version)) {
    throw new Error("Release version must match package.json, Cargo.toml and tauri.conf.json");
  }
  const [mode, target, input] = process.argv.slice(2);
  if (mode === "--configure") {
    const overlay = {
      plugins: { updater: { endpoints: [settings.endpoint], pubkey: settings.publicKey, requireSignedVersion: true } },
      bundle: { createUpdaterArtifacts: false },
    };
    if (process.platform === "win32") {
      Object.assign(overlay.bundle, JSON.parse(await readFile(path.join(root, ".native-windows.json"), "utf8")).bundle);
      overlay.bundle.windows = {
        certificateThumbprint: required("KIWIX_WINDOWS_CERTIFICATE_THUMBPRINT"),
        timestampUrl: validateBaseUrl(required("KIWIX_WINDOWS_TIMESTAMP_URL")).href,
        digestAlgorithm: "sha256",
      };
    }
    await writeFile(path.join(root, ".updater-release.json"), `${JSON.stringify(overlay, null, 2)}\n`);
    return;
  }
  let artifacts;
  if (mode === "--stage") {
    if (!targets.includes(target)) throw new Error(`Unsupported release target ${target}`);
    required("TAURI_SIGNING_PRIVATE_KEY");
    let file = path.resolve(input);
    if (!target.startsWith("darwin-")) {
      const suffix = target.startsWith("windows-") ? ".exe" : ".AppImage";
      const files = (await readdir(file)).filter((name) => name.endsWith(suffix));
      if (files.length !== 1) throw new Error(`Expected exactly one ${suffix} in ${file}`);
      file = path.join(file, files[0]);
    }
    artifacts = [{ target, file }];
  } else if (mode === "--manifest") {
    const directory = path.join(root, "release");
    const files = await readdir(directory);
    artifacts = targets.map((target) => {
      const matches = files.filter((file) => file.startsWith(`${target}-${settings.version}-`) && !file.endsWith(".sig"));
      if (matches.length !== 1) throw new Error(`Expected exactly one release artifact for ${target}`);
      return { target, file: path.join(directory, matches[0]) };
    });
  } else {
    throw new Error("Use --configure, --stage TARGET PATH, or --manifest");
  }
  await generate({
    version: settings.version, baseUrl: settings.baseUrl,
    out: path.join(root, mode === "--manifest" ? "published/update.json" : `release/${target}.json`),
    stageDir: path.join(root, mode === "--manifest" ? "published" : "release"),
    artifacts, publicKey: settings.publicKey, sign: mode === "--stage",
    verifyArtifact: verifyReleaseArtifact,
    privateKey: process.env.TAURI_SIGNING_PRIVATE_KEY,
    privateKeyPassword: process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ?? "",
    notesText: `Kiwix ${settings.version}. See the release page for release notes.`,
  });
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error) => { console.error(error.message); process.exitCode = 1; });
}
