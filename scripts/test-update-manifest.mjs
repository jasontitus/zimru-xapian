import assert from "node:assert/strict";
import { generateKeyPairSync, sign, createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import fs, { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { syncBuiltinESMExports } from "node:module";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import {
  buildManifest,
  generate,
  parseArtifact,
  tauriCli,
  validateBaseUrl,
  validateVersion,
  verifyArtifactSignature,
} from "./make-update-manifest.mjs";
import { releaseSettings } from "./stage-update-release.mjs";


function signingFixture(bytes, version = "0.1.1") {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const keyId = Buffer.from("0123456789abcdef", "hex");
  const publicRecord = Buffer.concat([
    Buffer.from("Ed"), keyId, publicKey.export({ type: "spki", format: "der" }).subarray(-32),
  ]);
  const payloadSignature = sign(null, createHash("blake2b512").update(bytes).digest(), privateKey);
  const record = Buffer.concat([Buffer.from("ED"), keyId, payloadSignature]);
  const comment = `timestamp:1\tfile:artifact\tversion:${version}`;
  const globalSignature = sign(null, Buffer.concat([payloadSignature, Buffer.from(comment)]), privateKey);
  return {
    publicKey: Buffer.from(`untrusted comment: key\n${publicRecord.toString("base64")}`).toString("base64"),
    signature: Buffer.from(`untrusted comment: signature\n${record.toString("base64")}\ntrusted comment: ${comment}\n${globalSignature.toString("base64")}`).toString("base64"),
  };
}

function optionsFor(directory, artifacts, extra = {}) {
  return {
    version: "0.1.1",
    baseUrl: "https://updates.example.org/kiwix/",
    out: path.join(directory, "out", "update.json"),
    artifacts,
    sign: false,
    ...extra,
  };
}
test("versions are semantic and targets use Tauri names", () => {
  assert.equal(validateVersion("0.1.1"), "0.1.1");
  assert.throws(() => validateVersion("latest"), /semantic version/);
  assert.deepEqual(parseArtifact("darwin-aarch64-app=/tmp/Kiwix.app"), {
    target: "darwin-aarch64-app",
    file: "/tmp/Kiwix.app",
  });
  assert.throws(
    () => parseArtifact("macos=Kiwix.app"),
    /Invalid target/,
  );
});

test("release versions match Rust semver syntax and u64 core bounds without normalization", () => {
  for (const version of [
    "0.0.0", "1.2.3-rc.1", "1.2.3+build.005", "1.2.3-rc.1+build.005",
    "1.2.3-0", "1.2.3--", "1.2.3-01a", "1.2.3-999999999999999999999999",
    "18446744073709551615.18446744073709551615.18446744073709551615",
  ]) {
    assert.equal(validateVersion(version), version);
  }
  for (const version of [
    "01.2.3", "1.02.3", "1.2.03", "1.2.3-01", "1.2.3-rc.01",
    "1.2.3-.", "1.2.3-rc..1", "1.2.3-rc.", "1.2.3+build..1", "1.2.3+",
    "1.2.3-", "1.2.3+build+other", "1.2.3-rc_1", "1.2.3-\u00e9",
    "1.2.3\n", "1.2.3\r\n", " 1.2.3", "v1.2.3", "1.2",
    "18446744073709551616.0.0", "0.18446744073709551616.0", "0.0.18446744073709551616",
    null, undefined, 123,
  ]) {
    assert.throws(() => validateVersion(version), /semantic version/, String(version));
  }
});

test("invalid versions fail generation before reading artifacts or publishing files", async () => {
  const directory = await mkdtemp(path.join(tmpdir(), "kiwix-version-"));
  try {
    await assert.rejects(generate(optionsFor(directory, [{
      target: "linux-x86_64-appimage", file: path.join(directory, "not-present.AppImage"),
    }], { version: "1.2.3-01" })), /semantic version/);
    assert.deepEqual(await readdir(directory), []);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("the endpoint must be HTTPS except for an explicit loopback proof", () => {
  assert.equal(
    validateBaseUrl("https://updates.example.org/kiwix/").href,
    "https://updates.example.org/kiwix/",
  );
  assert.throws(
    () => validateBaseUrl("http://updates.example.org/kiwix/"),
    /must use HTTPS/,
  );
  assert.equal(
    validateBaseUrl("http://127.0.0.1:8123/", { allowLoopback: true }).href,
    "http://127.0.0.1:8123/",
  );
  assert.throws(
    () => validateBaseUrl("http://192.0.2.4/", { allowLoopback: true }),
    /must use HTTPS/,
  );
});

test("a manifest carries every platform signature and a joined URL", () => {
  const manifest = buildManifest({
    version: "0.1.1",
    notes: "Fixes",
    pubDate: "2026-01-02T03:04:05Z",
    baseUrl: new URL("https://updates.example.org/kiwix/0.1.1/"),
    artifacts: [
      {
        target: "darwin-aarch64-app",
        fileName: "Kiwix.app.tar.gz",
        signature: "c2lnbmF0dXJl",
      },
      {
        target: "linux-x86_64-appimage",
        fileName: "Kiwix.AppImage.tar.gz",
        signature: "b3RoZXI=",
      },
    ],
  });
  assert.equal(manifest.version, "0.1.1");
  assert.equal(manifest.notes, "Fixes");
  assert.equal(manifest.pub_date, "2026-01-02T03:04:05.000Z");
  assert.equal(
    manifest.platforms["darwin-aarch64-app"].url,
    "https://updates.example.org/kiwix/0.1.1/Kiwix.app.tar.gz",
  );
  assert.equal(
    manifest.platforms["linux-x86_64-appimage"].signature,
    "b3RoZXI=",
  );
});

test("duplicate targets and missing signatures are rejected", () => {
  const baseUrl = new URL("https://updates.example.org/");
  assert.throws(
    () =>
      buildManifest({
        version: "0.1.1",
        baseUrl,
        artifacts: [
          { target: "darwin-aarch64-app", fileName: "a", signature: "x" },
          { target: "darwin-aarch64-app", fileName: "b", signature: "y" },
        ],
      }),
    /Duplicate target/,
  );
  assert.throws(
    () =>
      buildManifest({
        version: "0.1.1",
        baseUrl,
        artifacts: [
          { target: "darwin-aarch64-app", fileName: "a", signature: " " },
        ],
      }),
    /Missing signature/,
  );
});

test("generation copies an artifact, reads its .sig and writes the manifest", async () => {
  const directory = await mkdtemp(path.join(tmpdir(), "kiwix-manifest-"));
  try {
    const artifact = path.join(directory, "Kiwix_0.1.1_x64-setup.nsis.zip");
    await writeFile(artifact, "artifact bytes");
    const fixture = signingFixture(Buffer.from("artifact bytes"));
    await writeFile(`${artifact}.sig`, `${fixture.signature}\n`);
    const out = path.join(directory, "out", "update.json");
    const result = await generate({
      version: "0.1.1",
      baseUrl: "https://updates.example.org/kiwix/",
      out,
      artifacts: [
        {
          target: "windows-x86_64-nsis",
          file: artifact,
        },
      ],
      sign: false,
      privateKeyPath: null,
      publicKey: fixture.publicKey,
      privateKey: null,
      privateKeyPassword: null,
      allowLoopback: false,
      notesText: "Release",
      notesFile: null,
      pubDate: "2026-01-02T03:04:05Z",
    });
    const manifest = JSON.parse(await readFile(out, "utf8"));
    assert.equal(manifest.platforms["windows-x86_64-nsis"].signature, fixture.signature);
    assert.equal(
      manifest.platforms["windows-x86_64-nsis"].url,
      "https://updates.example.org/kiwix/windows-x86_64-nsis-0.1.1-Kiwix_0.1.1_x64-setup.nsis.zip",
    );
    assert.equal(result.manifest.version, "0.1.1");
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("a late publication collision preserves other outputs and permits a clean retry", async () => {
  const directory = await mkdtemp(path.join(tmpdir(), "kiwix-release-retry-"));
  const originalLink = fs.link;
  try {
    const bytes = Buffer.from("signed installer");
    const file = path.join(directory, "setup.exe");
    const fixture = signingFixture(bytes);
    await writeFile(file, bytes);
    await writeFile(`${file}.sig`, fixture.signature);
    const options = optionsFor(directory, [{ target: "windows-x86_64-nsis", file }], {
      publicKey: fixture.publicKey, stageDir: path.join(directory, "stage"),
      out: path.join(directory, "update.json"),
    });
    await writeFile(options.out, "previous release");
    await writeFile(`${options.out}.tmp`, "another process's file");
    // Simulate a competing publisher winning the signature name after our
    // artifact link succeeds. The real exclusive link then reports EEXIST.
    fs.link = async (source, destination) => {
      if (destination.endsWith(".sig")) await writeFile(destination, "other publisher", { flag: "wx" });
      return originalLink(source, destination);
    };
    syncBuiltinESMExports();
    await assert.rejects(generate(options), { code: "EEXIST" });
    assert.equal(await readFile(options.out, "utf8"), "previous release");
    const names = await readdir(options.stageDir);
    assert.deepEqual(names, ["windows-x86_64-nsis-0.1.1-setup.exe.sig"]);
    const collision = path.join(options.stageDir, names[0]);
    assert.equal(await readFile(collision, "utf8"), "other publisher");
    await rm(collision);
    fs.link = originalLink;
    syncBuiltinESMExports();
    await generate(options);
    assert.equal(JSON.parse(await readFile(options.out, "utf8")).version, "0.1.1");
    assert.equal(await readFile(`${options.out}.tmp`, "utf8"), "another process's file");
  } finally {
    fs.link = originalLink;
    syncBuiltinESMExports();
    await rm(directory, { recursive: true, force: true });
  }
});

test("source signatures authenticate exact bytes, trusted version and key", () => {
  const bytes = Buffer.from("signed release");
  const fixture = signingFixture(bytes);
  verifyArtifactSignature(bytes, fixture.signature, fixture.publicKey, "0.1.1");
  assert.throws(() => verifyArtifactSignature(Buffer.from("new bytes"), fixture.signature, fixture.publicKey, "0.1.1"), /verification failed/);
  assert.throws(() => verifyArtifactSignature(bytes, fixture.signature, fixture.publicKey, "0.1.2"), /release version/);
  assert.throws(() => verifyArtifactSignature(bytes, fixture.signature, signingFixture(bytes).publicKey, "0.1.1"), /verification failed/);
  const envelope = Buffer.from(fixture.signature, "base64").toString().replace("version:0.1.1", "version:0.1.2");
  assert.throws(() => verifyArtifactSignature(bytes, Buffer.from(envelope).toString("base64"), fixture.publicKey, "0.1.2"), /verification failed/);
});

test("rollback preserves replacements that arrive while a staged entry is being captured", async () => {
  const directory = await fs.realpath(await mkdtemp(path.join(tmpdir(), "kiwix-release-replacement-")));
  const originalLink = fs.link;
  const originalRename = fs.rename;
  try {
    const file = path.join(directory, "setup.exe");
    const stageDir = path.join(directory, "stage");
    const staged = path.join(stageDir, "windows-x86_64-nsis-0.1.1-setup.exe");
    const bytes = Buffer.from("signed fixture");
    const fixture = signingFixture(bytes);
    await writeFile(file, bytes);
    await writeFile(file + ".sig", fixture.signature);
    fs.link = async (source, destination) => {
      if (destination === staged + ".sig") await writeFile(destination, "unrelated signature", { flag: "wx" });
      return originalLink(source, destination);
    };
    fs.rename = async (source, destination) => {
      if (source !== staged) return originalRename(source, destination);
      await rm(staged);
      await writeFile(staged, "first replacement");
      await originalRename(source, destination);
      await writeFile(staged, "second replacement", { flag: "wx" });
    };
    syncBuiltinESMExports();
    await assert.rejects(generate(optionsFor(directory, [{ target: "windows-x86_64-nsis", file }],
      { stageDir, publicKey: fixture.publicKey })), /Changed entry preserved at/);
    const recovery = (await readdir(stageDir)).find(name => name.startsWith(".updater-rollback-"));
    assert.equal(await readFile(path.join(stageDir, recovery, path.basename(staged)), "utf8"), "first replacement");
    assert.equal(await readFile(staged, "utf8"), "second replacement");
    assert.equal(await readFile(staged + ".sig", "utf8"), "unrelated signature");
  } finally {
    fs.link = originalLink;
    fs.rename = originalRename;
    syncBuiltinESMExports();
    await rm(directory, { recursive: true, force: true });
  }
});

test("duplicate targets and output/input collisions fail before publishing", async () => {
  const directory = await mkdtemp(path.join(tmpdir(), "kiwix-collision-"));
  try {
    const file = path.join(directory, "release.AppImage");
    const bytes = Buffer.from("release");
    const fixture = signingFixture(bytes);
    await writeFile(file, bytes);
    await writeFile(`${file}.sig`, fixture.signature);
    const artifact = { target: "linux-x86_64-appimage", file };
    const options = optionsFor(directory, [artifact, artifact], { publicKey: fixture.publicKey });
    await assert.rejects(generate(options), /Duplicate target/);
    assert.deepEqual((await readdir(directory)).sort(), ["release.AppImage", "release.AppImage.sig"]);
    options.artifacts = [artifact];
    options.stageDir = path.join(directory, "stage");
    options.out = path.join(options.stageDir, "linux-x86_64-appimage-0.1.1-release.AppImage");
    await assert.rejects(generate(options), /Output collision/);
    assert.deepEqual((await readdir(directory)).sort(), ["release.AppImage", "release.AppImage.sig"]);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
test("Node-invoked Tauri signs and stages an installer with spaces and binds its prerelease and build version", async () => {
  const directory = await mkdtemp(path.join(tmpdir(), "kiwix installer signing "));
  try {
    const key = path.join(directory, "disposable key");
    execFileSync(process.execPath, [await tauriCli(), "signer", "generate", "-w", key, "-p", ""], { stdio: "pipe" });
    const publicKey = (await readFile(`${key}.pub`, "utf8")).trim();
    const file = path.join(directory, "Kiwix setup.exe");
    const bytes = Buffer.from("disposable installer fixture");
    await writeFile(file, bytes);
    const version = "0.1.1-rc.1+build.5";
    const options = optionsFor(directory, [{ target: "windows-x86_64-nsis", file }], {
      version, sign: true, privateKeyPath: key, privateKeyPassword: "", publicKey,
    });
    const originalKey = await readFile(key);
    await assert.rejects(generate({ ...options, out: key }), /collides with an input/);
    await assert.rejects(generate({ ...options, privateKeyPath: null, privateKey: key, out: key }), /collides with an input/);
    assert.deepEqual(await readFile(key), originalKey);
    const notes = path.join(directory, "release-notes");
    await writeFile(notes, "operator's release notes");
    await assert.rejects(generate({ ...options, notesFile: notes, out: notes }), /collides with an input/);
    assert.equal(await readFile(notes, "utf8"), "operator's release notes");
    const result = await generate(options);
    const artifact = result.manifest.platforms["windows-x86_64-nsis"];
    const staged = path.join(result.stageDir, decodeURIComponent(path.basename(new URL(artifact.url).pathname)));
    assert.deepEqual(await readFile(staged), bytes);
    assert.deepEqual(await readFile(file), bytes);
    assert.equal(result.manifest.version, version);
    verifyArtifactSignature(await readFile(staged), artifact.signature, publicKey, version);
    assert.throws(() => verifyArtifactSignature(bytes, artifact.signature, publicKey, "0.1.1-rc.1+build.6"), /version|verification failed/i);
    assert.throws(() => verifyArtifactSignature(bytes, artifact.signature, publicKey, "0.1.2"), /version|verification failed/i);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});


test("same basename architectures retain independent immutable payloads and signatures", async () => {
  const directory = await mkdtemp(path.join(tmpdir(), "kiwix-apps-"));
  try {
    const key = path.join(directory, "key");
    execFileSync(process.execPath, [await tauriCli(), "signer", "generate", "-w", key, "-p", ""], { stdio: "pipe" });
    const publicKey = (await readFile(`${key}.pub`, "utf8")).trim();
    const artifacts = [];
    for (const arch of ["aarch64", "x86_64"]) {
      const app = path.join(directory, arch, "Kiwix.app");
      await mkdir(app, { recursive: true });
      await writeFile(path.join(app, "payload"), arch);
      artifacts.push({ target: `darwin-${arch}-app`, file: app });
    }
    const options = optionsFor(directory, artifacts, {
      sign: true, privateKeyPath: key, privateKeyPassword: "", publicKey,
    });
    const result = await generate(options);
    const urls = artifacts.map(({ target }) => result.manifest.platforms[target].url);
    assert.notEqual(urls[0], urls[1]);
    const staged = urls.map((url) => path.join(result.stageDir, path.basename(new URL(url).pathname)));
    for (const [index, file] of staged.entries()) {
      const bytes = await readFile(file);
      const signature = result.manifest.platforms[artifacts[index].target].signature;
      verifyArtifactSignature(bytes, signature, publicKey, "0.1.1");
      const payload = execFileSync("tar", ["-xOzf", file, "Kiwix.app/payload"], { encoding: "utf8" });
      assert.equal(payload, index === 0 ? "aarch64" : "x86_64");
    }
    const original = await readFile(staged[0]);
    await writeFile(path.join(artifacts[0].file, "payload"), "replaced");
    await assert.rejects(generate(options), /Output already exists/);
    assert.deepEqual(await readFile(staged[0]), original);
    await assert.rejects(generate({ ...options, stageDir: path.join(directory, "no-sign"), sign: false }), /freshly packaged bytes must be signed/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("a stale staged signature cannot authorize changed source bytes", async () => {
  const directory = await mkdtemp(path.join(tmpdir(), "kiwix-stale-signature-"));
  try {
    const file = path.join(directory, "release.AppImage");
    const fixture = signingFixture(Buffer.from("old bytes"));
    await writeFile(file, "new bytes");
    await writeFile(`${file}.sig`, fixture.signature);
    await assert.rejects(generate(optionsFor(directory, [{ target: "linux-x86_64-appimage", file }], {
      publicKey: fixture.publicKey,
    })), /verification failed/);
    assert.deepEqual((await readdir(directory)).sort(), ["release.AppImage", "release.AppImage.sig"]);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("release configuration requires the real trust key and derives the repository channel", () => {
  const fixture = signingFixture(Buffer.from("release"));
  const env = {
    KIWIX_RELEASE_VERSION: "0.1.1",
    GITHUB_REPOSITORY: "kiwix/kiwix-workbench",
    KIWIX_UPDATER_PUBLIC_KEY: fixture.publicKey,
  };
  const settings = releaseSettings(env);
  assert.equal(settings.endpoint, "https://github.com/kiwix/kiwix-workbench/releases/latest/download/update.json");
  assert.equal(settings.baseUrl, "https://github.com/kiwix/kiwix-workbench/releases/download/v0.1.1/");
  assert.throws(() => releaseSettings({ ...env, KIWIX_UPDATER_PUBLIC_KEY: "REPLACE_WITH_TAURI_UPDATER_PUBLIC_KEY" }), /real base64 Tauri minisign public key/);
  for (const repository of ["../untrusted", "kiwix/..", "kiwix/.", "kiwix/repository/extra", "-kiwix/repository", "kiwix/repository?query"]) {
    assert.throws(() => releaseSettings({ ...env, GITHUB_REPOSITORY: repository }), /GITHUB_REPOSITORY/);
  }
  assert.throws(() => releaseSettings({ ...env, KIWIX_RELEASE_VERSION: "latest" }), /semantic version/);
  assert.throws(() => releaseSettings({ ...env, KIWIX_RELEASE_VERSION: "1.2.3-01" }), /semantic version/);
  const prerelease = releaseSettings({ ...env, KIWIX_RELEASE_VERSION: "1.2.3-rc.1+build.5" });
  assert.equal(prerelease.version, "1.2.3-rc.1+build.5");
  assert.equal(prerelease.baseUrl, "https://github.com/kiwix/kiwix-workbench/releases/download/v1.2.3-rc.1+build.5/");
});

test("artifact URLs preserve filename bytes rather than URL query or fragment syntax", () => {
  const manifest = buildManifest({
    version: "0.1.1", baseUrl: new URL("https://updates.example.org/releases/"),
    artifacts: [{ target: "linux-x86_64-appimage", fileName: "Kiwix #1?.AppImage", signature: "signed" }],
  });
  const url = new URL(manifest.platforms["linux-x86_64-appimage"].url);
  assert.equal(decodeURIComponent(path.basename(url.pathname)), "Kiwix #1?.AppImage");
  assert.equal(url.search, "");
  assert.equal(url.hash, "");
});

test("downloaded package bytes are authenticated before inspection and inspected in private staging", async () => {
  const directory = await mkdtemp(path.join(tmpdir(), "kiwix-verified-package-"));
  try {
    const bytes = Buffer.from("owned package fixture");
    const { signature, publicKey } = signingFixture(bytes);
    const file = path.join(directory, "fixture.AppImage");
    await writeFile(file, Buffer.from("changed package fixture"));
    await writeFile(file + ".sig", signature);
    let count = 0;
    const options = optionsFor(directory, [{ target: "linux-x86_64-appimage", file }], {
      publicKey,
      async verifyArtifact(target, staged) {
        count++;
        assert.equal(target, "linux-x86_64-appimage");
        assert.notEqual(staged, file);
        assert.deepEqual(await readFile(staged), bytes);
      },
    });
    await assert.rejects(generate(options), /signature|authentic/i);
    assert.equal(count, 0);
    await writeFile(file, bytes);
    await generate(options);
    assert.equal(count, 1);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
