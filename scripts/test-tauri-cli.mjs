// SPDX-License-Identifier: GPL-3.0-or-later
import assert from "node:assert/strict";
import test from "node:test";
import { linuxPlan } from "./tauri-cli.mjs";

const native = { platform: "linux", arch: "x64", envTarget: "" };
const plan = (...args) => linuxPlan(args, native);
const overlay = (bundle) => JSON.stringify({ bundle });

test("the standard Linux commands retain the requested packages and trailing Cargo options", () => {
  assert.deepEqual(plan("build").args, ["build", "--bundles", "deb,rpm"]);
  for (const args of [["-bappimage"], ["-b=appimage"], ["--bundles=appimage"], ["--bundles", "appimage"]]) {
    assert.deepEqual(plan("bundle", ...args, "--", "--locked").args, ["bundle", "--bundles", "deb", "--", "--locked"]);
  }
  assert.deepEqual(plan("build", "--bundles", "appimage,deb", "rpm").args, ["build", "--bundles", "deb,rpm"]);
  assert.deepEqual(plan("-vv", "build", "-bappimage").args, ["-vv", "build", "--bundles", "deb"]);
  assert.throws(() => plan("bundle", "-vdbappimage"), /separate short options/);
  assert.equal(plan("build", "-bdeb"), null);
  assert.equal(plan("build", "--no-bundle"), null);
  assert.equal(plan("build", "-t=x86_64-pc-windows-msvc"), null);
  assert.equal(plan("build", "-tx86_64-unknown-linux-gnu").target, "x86_64-unknown-linux-gnu");
  assert.throws(() => linuxPlan(["build"], { ...native, envTarget: "aarch64-unknown-linux-gnu" }), /x86-64/);
  assert.throws(() => plan("bundle", "-bappimage", "--no-binary-patching"), /bundle type/);
});

test("configuration overlays merge active state and target selection separately", () => {
  const disabled = overlay({ active: false, targets: ["appimage"] });
  assert.equal(plan("build", "-c", disabled), null);
  const config = ["-c", disabled, "--config", overlay({ active: true })];
  assert.deepEqual(plan("build", ...config).args, ["build", ...config, "--bundles", "deb"]);
  assert.equal(plan("build", "-c" + overlay({ targets: ["rpm"] })), null);
});
