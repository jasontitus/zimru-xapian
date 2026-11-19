#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Loopback acceptance run for the signed updater.
//
// It generates a throwaway signing key, signs a second macOS app or Linux
// AppImage, serves the manifest and artifacts from 127.0.0.1, and runs the ignored
// Rust test that checks, downloads, verifies, installs and rejects a tampered
// artifact. Nothing here uses a production key.

import { spawn, spawnSync } from "node:child_process";
import {
  cp,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { buildManifest, generate, verifyReleaseArtifact } from "./make-update-manifest.mjs";

const repository = fileURLToPath(new URL("..", import.meta.url));
const linux = process.platform === "linux" && process.arch === "x64";
const target = process.env.KIWIX_UPDATE_E2E_TARGET ?? (linux ? "linux-x86_64-appimage" : `darwin-${process.arch === "arm64" ? "aarch64" : "x86_64"}-app`);

const usage = `Usage: node scripts/update-e2e.mjs --app0 OLD --app1 NEW [options]
  --version0 VERSION       Old app version (default: 0.1.0)
  --version1 VERSION       New app version (default: 0.1.1)
  --work-dir DIR           Work directory (default: .native/update-e2e)
  --target TARGET          Updater target key (default: ${target})
  --port PORT              Loopback port (default: an available port)
  --keep                   Keep the work directory

The two apps must already be built. The script never touches tauri.conf.json
or uses a production signing key.`;

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: repository,
    stdio: "inherit",
    ...options,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} failed (${result.status})`);
  }
  return result;
}

function parseArgs(argv) {
  const options = {
    app0: null,
    app1: null,
    version0: "0.1.0",
    version1: "0.1.1",
    workDir: path.join(repository, ".native", "update-e2e"),
    target,
    port: 0,
    keep: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    switch (argv[index]) {
      case "--app0":
        options.app0 = argv[++index];
        break;
      case "--app1":
        options.app1 = argv[++index];
        break;
      case "--version0":
        options.version0 = argv[++index];
        break;
      case "--version1":
        options.version1 = argv[++index];
        break;
      case "--work-dir":
        options.workDir = path.resolve(argv[++index]);
        break;
      case "--target":
        options.target = argv[++index];
        break;
      case "--port":
        options.port = Number(argv[++index]);
        break;
      case "--keep":
        options.keep = true;
        break;
      case "--help":
      case "-h":
        console.log(usage);
        process.exit(0);
        break;
      default:
        throw new Error(`Unknown option ${argv[index]}\n\n${usage}`);
    }
  }
  if (!options.app0 || !options.app1) throw new Error(usage);
  return options;
}

function staticServer(root) {
  const server = http.createServer(async (request, response) => {
    let requested;
    try { requested = decodeURIComponent((request.url ?? "/").split("?")[0]); }
    catch { response.writeHead(400).end(); return; }
    const file = path.resolve(root, `.${requested}`);
    if (!file.startsWith(`${path.resolve(root)}${path.sep}`)) {
      response.writeHead(403).end("forbidden");
      return;
    }
    try {
      const body = await readFile(file);
      response.writeHead(200, {
        "content-type": file.endsWith(".json")
          ? "application/json"
          : "application/octet-stream",
        "content-length": body.length,
        "cache-control": "no-store",
      });
      response.end(body);
    } catch {
      response.writeHead(404).end("not found");
    }
  });
  return server;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (!linux && process.platform !== "darwin") throw new Error("This install harness requires macOS or x86-64 Linux.");
  if (linux && options.target !== "linux-x86_64-appimage") throw new Error("Expected linux-x86_64-appimage on this host");
  await mkdir(options.workDir, { recursive: true });
  const workDir = await mkdtemp(path.join(options.workDir, "run-"));
  const linuxEnvironment = linux ? { TMPDIR: workDir, APPIMAGE_EXTRACT_AND_RUN: "1" } : {};
  const keyPath = path.join(workDir, "test-updater.key");
  const publicDir = path.join(workDir, "public");
  const server = staticServer(publicDir);
  try {
    for (const [app, version] of [[options.app0, options.version0], [options.app1, options.version1]]) {
      if (linux) {
        await verifyReleaseArtifact(options.target, path.resolve(app));
        const actual = run(path.resolve(app), ["--version"], { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"], env: { ...process.env, ...linuxEnvironment } });
        if (actual.stdout.trim() !== `Kiwix Rust ${version}`) throw new Error(`AppImage must report version ${version}`);
      } else {
        const plist = path.join(path.resolve(app), "Contents", "Info.plist");
        const actual = spawnSync("/usr/libexec/PlistBuddy", ["-c", "Print :CFBundleShortVersionString", plist], { encoding: "utf8" });
        if (actual.status !== 0 || actual.stdout.trim() !== version) throw new Error(`App ${app} must report version ${version}`);
      }
    }
    await mkdir(path.join(workDir, "installed"), { recursive: true });
    await cp(path.resolve(options.app0), path.join(workDir, "installed", linux ? "Kiwix.AppImage" : "Kiwix.app"), { recursive: true, verbatimSymlinks: true });

    await mkdir(publicDir);
    run(process.execPath, [
      path.join(repository, "node_modules/@tauri-apps/cli/tauri.js"),
      "signer",
      "generate",
      "-w",
      keyPath,
      "-p",
      "",
    ]);
    const publicKey = (await readFile(`${keyPath}.pub`, "utf8")).trim();

    // Start the server on an ephemeral port, then build URLs from it.
    await new Promise((resolve, reject) => {
      server.once("error", reject);
      server.listen(options.port, "127.0.0.1", resolve);
    });
    const port = server.address().port;
    const baseUrl = `http://127.0.0.1:${port}/`;

    const result = await generate({
      verifyArtifact: verifyReleaseArtifact,
      version: options.version1,
      baseUrl,
      out: path.join(publicDir, "update.json"),
      stageDir: publicDir,
      artifacts: [{ target: options.target, file: options.app1 }],
      notesText: `Kiwix ${options.version1} acceptance build`,
      notesFile: null,
      pubDate: new Date().toISOString(),
      sign: true,
      privateKeyPath: keyPath,
      privateKey: null,
      privateKeyPassword: "",
      publicKey,
      allowLoopback: true,
    });
    const signature = result.manifest.platforms[options.target].signature;
    const artifactName = decodeURIComponent(path.basename(
      new URL(result.manifest.platforms[options.target].url).pathname,
    ));
    const artifact = await readFile(path.join(publicDir, artifactName));
    const tampered = Buffer.from(artifact);
    tampered[tampered.length - 1] ^= 0xff;
    await writeFile(path.join(publicDir, "tampered.tar.gz"), tampered);
    const tamperedManifest = buildManifest({
      version: options.version1,
      notes: "Tampered acceptance artifact",
      pubDate: new Date().toISOString(),
      baseUrl: new URL(baseUrl),
      artifacts: [
        {
          target: options.target,
          fileName: "tampered.tar.gz",
          signature,
        },
      ],
    });
    await writeFile(
      path.join(publicDir, "tampered.json"),
      `${JSON.stringify(tamperedManifest, null, 2)}\n`,
    );

    const test = spawn("cargo", [
      "test",
      "--locked",
      ...(linux ? ["--release"] : []),
      "--lib",
      "--features",
      linux ? "custom-protocol" : "localswarm-profiling",
      "update::tests::loopback_end_to_end",
      "--",
      "--ignored",
      "--exact",
      "--nocapture",
    ], {
      cwd: path.join(repository, "src-tauri"),
      env: {
        ...process.env,
        ...linuxEnvironment,
        KIWIX_UPDATE_E2E_MANIFEST: `${baseUrl}update.json`,
        KIWIX_UPDATE_E2E_TAMPERED_MANIFEST: `${baseUrl}tampered.json`,
        KIWIX_UPDATE_E2E_PUBKEY: publicKey,
        KIWIX_UPDATE_E2E_WORKDIR: workDir,
        KIWIX_UPDATE_E2E_VERSION: options.version1,
        KIWIX_UPDATE_E2E_TARGET: options.target,
      },
      stdio: "inherit",
    });
    const status = await new Promise((resolve, reject) => {
      test.on("error", reject);
      test.on("exit", resolve);
    });
    if (status !== 0) throw new Error(`cargo test exited ${status}`);

    console.log(JSON.stringify({
      result: "passed",
      target: options.target,
      version0: options.version0,
      version1: options.version1,
      manifest: `${baseUrl}update.json`,
      artifact: `${baseUrl}${artifactName}`,
      tamperedManifest: `${baseUrl}tampered.json`,
      publicKey: `${keyPath}.pub`,
      workDir,
      note: linux ? "Verified and launched the installed AppImage with --version." : "Verified the replaced temporary app bundle.",
    }, null, 2));
  } finally {
    server.close();
    await rm(keyPath, { force: true });
    if (!options.keep) await rm(workDir, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error.stack ?? String(error));
  process.exit(1);
});
