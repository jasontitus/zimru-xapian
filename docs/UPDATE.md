# Signed desktop updates

Kiwix Workbench uses the Tauri v2 updater on macOS, Windows and Linux. The
updater is a **distribution responsibility**, not something this repository can
finish by itself: a distributor must supply a long-lived signing key, a notarized
macOS build (and equivalent signed Windows/Linux packages), and an HTTPS update
endpoint. Mobile builds do not self-update at all.

The implementation is deliberately narrow:

- `src-tauri/src/update.rs` owns policy, status, progress and persistence.
- `tauri-plugin-updater` owns manifest parsing, artifact download, minisign
  signature verification and installation.
- `scripts/make-update-manifest.mjs` packages/signs artifacts and writes the
  static Tauri updater JSON.
- `src/components/UpdatesSettings.tsx` is the Settings surface.

The page never calls the plugin's JavaScript `check`, `download` or `install`
commands. No updater permission is granted in `src-tauri/capabilities/`. The
shell calls `update_status`, `update_check` and `update_install`, which pass
through the same dispatch boundary as every other command, so privacy mode,
read-only mode, window ownership and shutdown checks cannot be bypassed from
the webview.

## Trust model

1. **The artifact signature is the authority.** The manifest is fetched over
   HTTPS but is not itself signed. Each platform entry carries a minisign
   signature over the artifact bytes. The public key is compiled into
   `tauri.conf.json` (`plugins.updater.pubkey`). A changed manifest can point at
   a different URL, but it cannot produce a valid signature for different bytes.
2. **The signature is bound to the announced version.**
   `requireSignedVersion: true` makes the updater read the version from the
   signature's trusted comment (written by `tauri signer sign --app-version`)
   and reject a manifest that announces a different version. This prevents a
   tampered manifest from pairing a newer version number with an older, validly
   signed artifact.
3. **Nothing installs without a person choosing it.** The daily background check
   only fetches the manifest. It never downloads an artifact. `update_install`
   requires an explicit confirmation in Settings and then downloads, verifies,
   installs and restarts. A failed signature check aborts with a plain-language
   message and leaves the running build in place.
4. **Privileged actions respect the profile.** `private_mode` and `read_only`
   refuse with an explanation instead of silently hiding the control. The check
   is still a network request, and installation changes the application, so both
   are privileged.
5. **No restart during a write.** Installation owns a shared admission writer
   before transfer/privacy locks, checks existing HTTP/torrent/LocalSwarm/map
   workers and migration activity, then retains exclusion through artifact
   download, verification, install and restart. Transfer starts, automatic
   recovery and migration consumers use the same gate. Paused rows without
   workers and read-only seeds do not by themselves imply a pending write.
6. **One update operation at a time.** A second check or install receives an
   explicit "already being checked or installed" message.
7. **Metered networks.** Automatic checks reuse the transfer policy's Internet
   admission and the saved metered-download allowance. The updater never
   downloads automatically at all, so a metered connection cannot incur update
   traffic without the person choosing **Install update**. Desktop operating
   systems do not expose a portable metered flag here; the mobile policy is the
   only place that can currently classify a connection as metered.
8. **Last check is durable.** `update-state.json` in the profile stores the last
   check time, the version seen, and the last plain-language error. It is never
   written in private or read-only mode. The daily check is due only when that
   timestamp is at least 24 hours old.

## What the distributor must supply

| Item | Detail |
| --- | --- |
| Updater keypair | Generate and escrow one long-lived key with `npx tauri signer generate -w updater.key`. Set protected environment variable `KIWIX_UPDATER_PUBLIC_KEY` to the real Tauri public-key file contents and secrets `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` to its matching private key/password. Release tooling merges the key into configuration. |
| Key rotation plan | Existing builds trust only the key compiled into them. To rotate, first ship a build that trusts the new key, wait for adoption, then start signing with the new key. Losing the private key means existing installs cannot be updated in place. |
| HTTPS host | Base development configuration has empty `pubkey` and `endpoints`. Release tooling derives `https://github.com/<GITHUB_REPOSITORY>/releases/latest/download/update.json` from the actual repository; it never assumes ownership of an updates.kiwix.org host. |
| macOS signing/notarization | A Developer ID Application certificate, hardened runtime, the existing entitlements, and notarization. Ad-hoc local builds can be updated only by an ad-hoc artifact and are not a distributable release. |
| Windows signing | A code-signing certificate for the NSIS/MSI installer. The updater launches the installer; an unsigned installer is blocked or warned by SmartScreen. |
| Linux packaging | An AppImage, `.deb` or `.rpm` built for the target distribution. The updater chooses the matching artifact. |
| Store accounts | App Store Connect and Google Play Console for mobile updates. These are not in-app updater feeds. |
| Release notes | Plain text (no HTML required). The Settings panel renders them as text, preserving line breaks. |

Private keys are never committed. Unconfigured development builds report that
updates were not set up; no placeholder or generated throwaway key is a release
trust root. Previously installed builds without the real key/channel need an
initial correctly configured distribution before they can use that channel.

## Release procedure

1. **Bump the version.** Update `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`
   and `package.json`. The updater compares the running package version with the
   manifest version.
2. **Build and sign the desktop artifacts.**
   - macOS: `npm run tauri -- build --bundles app`, then
     `node scripts/bundle-macos.mjs <Kiwix.app> --identity "Developer ID Application: …" --notarize-profile <profile>`.
     The staged updater filename includes target, version and basename, with
     `Kiwix.app` at the tarball root. Both macOS architectures retain distinct bytes.
   - Windows: protected workflow signs NSIS with the provisioned certificate.
   - Linux: protected workflow audits the system-runtime AppImage and signs its exact bytes.
3. **Sign each updater artifact with its version.**
   ```sh
   npx tauri signer sign --private-key-path updater.key \
     --app-version 0.1.1 Kiwix.app.tar.gz
   ```
   The manifest script signs freshly prepared artifact bytes. Repackaged `.app`
   bytes always require signing; it never reuses a stale staging signature.
4. **Generate the manifest.**
   ```sh
   node scripts/make-update-manifest.mjs \
     --version 0.1.1 \
     --notes release-notes.txt \
     --base-url https://updates.example.org/kiwix/0.1.1/ \
     --artifact darwin-aarch64-app=Kiwix.app \
     --artifact windows-x86_64-nsis=Kiwix_0.1.1_x64-setup.exe \
     --artifact linux-x86_64-appimage=org.kiwix.workbench_0.1.1_amd64.AppImage \
     --stage-dir dist/artifacts-0.1.1 --out dist/update.json
   ```
   Use a fresh stage directory. Duplicate targets, output/input collisions and
   existing immutable outputs fail before publication. For an already packaged
   signed artifact, `--no-sign --public-key <real-public-key>` cryptographically
   verifies its source `.sig` against exact bytes and trusted version. This mode
   refuses `.app` packaging. HTTP is allowed only by explicit loopback proof.
5. **Upload atomically.** Upload the artifacts first, then replace
   `update.json`. Keep the previous release's artifacts available so a client
   that checked just before the switch can still download what its manifest
   named.
6. **Verify from the previous build.** Install version *N−1*, point it at the
   endpoint (or use the loopback proof below), check, confirm the signature
   failure path with a deliberately corrupted artifact, then install *N* and
   confirm the version after restart.
7. **Keep the channel stable.** The Settings panel names the `stable` channel.
   A beta channel would be another endpoint and another public key or target
   path; do not silently switch a build's endpoint.

Release trust comes only from compiled configuration (including the generated
`.updater-release.json` overlay). `KIWIX_UPDATER_ENDPOINT` and
`KIWIX_UPDATER_PUBKEY` overrides are accepted only in debug acceptance builds,
never by the release runtime.

### Protected release workflow

`.github/workflows/build.yml` stages native arm64/Intel macOS, Windows NSIS and
Linux AppImage artifacts into a GitHub **draft** release. The version must match
package/Cargo/Tauri files and an existing `vVERSION` tag on the selected main
commit. Publishing the accepted draft is a distributor action.

Configure protected environment `desktop-release` with the updater key above,
Developer ID/notarization secrets `APPLE_CERTIFICATE_P12`,
`APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`,
`APPLE_PASSWORD`, `APPLE_TEAM_ID`; and a trusted Windows runner labelled
`[self-hosted, Windows, X64, kiwix-libzim-9_8]` with the pinned SDK, signtool
and accessible Authenticode private key. Set
`KIWIX_WINDOWS_CERTIFICATE_THUMBPRINT` and a real HTTPS
`KIWIX_WINDOWS_TIMESTAMP_URL`. Windows release execution is explicitly enabled.
Signing credentials and runner provisioning are not available in this checkout.

`node scripts/stage-update-release.mjs --configure` creates the release overlay;
`--stage TARGET PATH` signs final platform artifacts; `--manifest` verifies and
assembles all four exact target artifacts. The workflow supplies real
`GITHUB_REPOSITORY`, `KIWIX_RELEASE_VERSION` and protected keys. No ad-hoc,
unsigned or generated-key release fallback exists.

## Platform reality

| Platform | Behaviour |
| --- | --- |
| macOS | Self-update through the plugin. The `.app` must be signed and notarized for distribution. The updater swaps the bundle and the app restarts. |
| Windows | Self-update through the NSIS/MSI installer. The installer is code-signed; the app exits while it runs. |
| Linux | Self-update for AppImage, `.deb` and `.rpm`. Package-manager installs may prompt for privileges. |
| iOS / iPadOS | No in-app self-update. Updates arrive through the App Store (or TestFlight). The Settings panel says so and offers no button. |
| Android | No in-app self-update. Updates arrive through Google Play, or through whatever installer supplied the APK (for example a sideloaded build). The Settings panel says so and offers no button. |

## Local loopback acceptance proof

This proves the client, manifest and signature path without production
credentials. It uses a throwaway key and an HTTP loopback endpoint; release
builds reject that endpoint by design.

```sh
# 1. Build two macOS apps. The second must have version 0.1.1.
npm run tauri -- build --debug --bundles app
cp -R src-tauri/target/debug/bundle/macos/Kiwix.app /tmp/Kiwix-0.1.0.app
# bump both versions to 0.1.1, rebuild, then copy the new app:
cp -R src-tauri/target/debug/bundle/macos/Kiwix.app /tmp/Kiwix-0.1.1.app
# restore the versions after the proof

# 2. Run the harness. It generates a test key, signs the 0.1.1 app, serves the
#    manifest/artifact on 127.0.0.1, and runs the ignored Rust test.
node scripts/update-e2e.mjs \
  --app0 /tmp/Kiwix-0.1.0.app \
  --app1 /tmp/Kiwix-0.1.1.app \
  --version0 0.1.0 --version1 0.1.1 --keep
```

On Ubuntu 24.04 x86-64, pass two system-runtime AppImages as `--app0` and `--app1` with their actual versions. The harness audits both images before launching them and signs the newer image with a temporary key. The native test installs it into a copy of the older image, compares the installed bytes and launches the replacement with `--version`. The test server serves only public artifacts and deletes its private key after the run.

The Rust test performs:

- `check()` against the loopback manifest and asserts version `0.1.1`;
- `download()` with progress callbacks and a non-empty byte count;
- `install()` into a temporary `.app` and asserts the new bundle replaced it;
- a second manifest whose artifact bytes were flipped while keeping the original
  signature, and asserts the download fails with a minisign error;
- a state-file round trip proving `lastChecked` persists and suppresses a second
  check within 24 hours.

The mock Tauri runtime cannot restart a process, so the final `app.restart()` in
the production command is not exercised by this test. It is a single call after
a successful install; the installed bundle replacement is verified. A real
windowed 0.1.0 → 0.1.1 restart still needs a signed/notarized release build and
is not claimed here.

## Failure messages

| Situation | What the person sees |
| --- | --- |
| Bad signature / tampered artifact | "Kiwix could not confirm that the update came from Kiwix, so it kept the version you have. Download the update again from the official Kiwix site." |
| No update information | "No update information was found at the update address. Try again later." |
| Offline / server unreachable | "Kiwix could not reach the update service. Check your connection, then try again." |
| Wrong platform entry | "This version of Kiwix does not have an update for this computer…" |
| Private / read-only | The specific mode explanation, with the action to leave it. |
| Download or archive move in progress | The specific transfer/move message; the install button is disabled. |
| Build without a release key | "This build was not set up for updates…" |

These messages describe the refused action and available recovery. Signature and
release-address validation are covered by `npm run test:update-manifest`.

## What remains unverified

- A production Developer ID/notarized macOS update and a signed Windows
  installer. The loopback proof uses debug/ad-hoc artifacts.
- The actual `app.restart()` after install in a real bundle (the install itself
  is verified by the loopback test).
- Play Store / App Store submission. Mobile has no self-update by design.
- A metered desktop connection. No portable desktop metered signal exists in
  this build; automatic checks never download, and the metered rule is unit
  tested for the mobile policy shape.
