// SPDX-License-Identifier: GPL-3.0-or-later
// Preserve the normal Tauri command interface. The supported Linux AppImage
// uses the same Debian staging bundle as the system-runtime installer.
import { spawnSync } from "node:child_process";
import { readFileSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const cli = path.join(root, "node_modules/@tauri-apps/cli/tauri.js");

function run(command, args, options = {}) {
  const child = spawnSync(command, args, { cwd: root, stdio: "inherit", ...options });
  if (child.error) throw child.error;
  if (child.status !== 0) process.exit(child.status ?? 1);
  return child.stdout;
}

function debs(directory) {
  try {
    return new Map(readdirSync(directory).filter(n => n.endsWith(".deb")).map(name => {
      const stat = statSync(path.join(directory, name), { bigint: true });
      return [name, [stat.ino, stat.size, stat.ctimeNs].join("-")];
    }));
  } catch (error) {
    if (error.code === "ENOENT") return new Map();
    throw error;
  }
}

function overlayTargets(configs) {
  let active = true;
  let selection;
  for (const config of configs) {
    let data;
    try {
      data = JSON.parse(config.trimStart().startsWith("{") ? config : readFileSync(path.resolve(root, config), "utf8"));
    } catch {
      throw new Error("Choose --bundles explicitly when using a non-JSON Tauri configuration.");
    }
    if (data.bundle?.active !== undefined) active = data.bundle.active;
    if (data.bundle?.targets !== undefined) selection = [data.bundle.targets].flat();
  }
  return active ? selection : [];
}

export function linuxPlan(args, { platform = process.platform, arch = process.arch, envTarget = process.env.CARGO_BUILD_TARGET } = {}) {
  const at = args.indexOf("--");
  const options = at < 0 ? args : args.slice(0, at);
  const command = options.find(arg => arg !== "--verbose" && !/^-v+$/.test(arg));
  if (platform !== "linux" || !["build", "bundle"].includes(command) ||
      options.some(s => ["--no-bundle", "-h", "--help", "-V", "--version"].includes(s))) return null;
  let selected = [];
  const configs = [];
  const forwarded = [];
  let target;
  for (let i = 0; i < options.length; i++) {
    const arg = options[i];
    if (/^-[vdhV][^-]+/.test(arg) && !/^-v+$/.test(arg)) {
      throw new Error("Use separate short options for a Linux package build (for example -v -d -b appimage).");
    }
    if (arg === "--bundles" || arg === "-b") {
      while (options[i + 1] && !options[i + 1].startsWith("-")) selected.push(...options[++i].split(","));
    } else if (arg.startsWith("-b") && !arg.startsWith("--") && arg.length > 2) {
      selected.push(...arg.slice(2).replace(/^=/, "").split(","));
    } else if (arg.startsWith("--bundles=")) {
      selected.push(...arg.slice("--bundles=".length).split(","));
    } else {
      forwarded.push(arg);
      if (arg === "-t" || arg === "--target") target = options[i + 1];
      if (arg.startsWith("--target=")) target = arg.slice(9);
      if (arg.startsWith("-t") && !arg.startsWith("--") && arg.length > 2) target = arg.slice(2).replace(/^=/, "");
      if (arg === "-c" || arg === "--config") configs.push(options[i + 1]);
      if (arg.startsWith("--config=")) configs.push(arg.slice(9));
      if (arg.startsWith("-c") && !arg.startsWith("--") && arg.length > 2) configs.push(arg.slice(2).replace(/^=/, ""));
    }
  }
  target ??= envTarget;
  if (target && !target.includes("linux")) return null;
  if (!selected.length) {
    const fromConfig = overlayTargets(configs);
    if (fromConfig && !fromConfig.includes("appimage") && !fromConfig.includes("all")) return null;
    if (fromConfig && !fromConfig.includes("all")) selected = fromConfig;
  }
  if (selected.length && !selected.includes("appimage")) return null;
  if (options.includes("--no-binary-patching")) throw new Error("An AppImage update requires the AppImage bundle type in the executable. Remove --no-binary-patching or request a different artifact.");
  if (arch !== "x64" || (target && target !== "x86_64-unknown-linux-gnu"))
    throw new Error("The system-runtime AppImage supports Ubuntu 24.04 x86-64. Choose --bundles deb for another Linux package.");
  const wanted = selected.length ? selected.map(s => s === "appimage" ? "deb" : s) : ["deb", "rpm"];
  const profile = options.includes("--debug") || options.includes("-d") ? "debug" : "release";
  return { target, profile, args: [...forwarded, "--bundles", [...new Set(wanted)].join(","), ...(at < 0 ? [] : args.slice(at))] };
}

function main(args) {
  const plan = linuxPlan(args);
  if (!plan) return run(process.execPath, [cli, ...args]);
  const native = path.join(root, "src-tauri");
  const metadata = JSON.parse(run("cargo", ["metadata", "--format-version", "1", "--no-deps", "--locked"],
    { cwd: native, encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] }));
  const output = path.join(metadata.target_directory, plan.target ?? "", plan.profile);
  const directory = path.join(output, "bundle/deb");
  const prior = debs(directory);
  run(process.execPath, [cli, ...plan.args]);
  const fresh = [...debs(directory)].filter(([n, id]) => prior.get(n) !== id);
  if (fresh.length !== 1) throw new Error("Expected exactly one newly generated Debian bundle from this Tauri run.");
  const file = fresh[0][0];
  const image = path.join(output, "bundle/appimage", file.slice(0, -4) + ".AppImage");
  run("python3", [path.join(root, "scripts/linux-appimage.py"), "assemble", path.join(directory, file),
    path.join(output, "kiwix-workbench"), path.join(native, "binaries/kiwix-mcp-x86_64-unknown-linux-gnu"), image]);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
