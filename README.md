# Kiwix — your library, offline

**Kiwix is an offline reader for ZIM archives** — self-contained copies of Wikipedia, Wiktionary, Stack Exchange, medical references, course material, and millions of other pages. Download a book once, then read it on a plane, on a train, or anywhere the network does not reach.

This repository is an independent **Rust + Tauri** implementation for macOS, Windows, Linux, iOS, and Android. It is not an official Kiwix release. The official readers — [Kiwix for Apple](https://github.com/kiwix/kiwix-apple), [Kiwix for Android](https://github.com/kiwix/kiwix-android), [Kiwix Desktop](https://github.com/kiwix/kiwix-desktop), and [Kiwix JS](https://github.com/kiwix/kiwix-js) — remain the reference apps for their platforms. This project borrows the best ideas from each and adds a few of its own.

No account is required. Your library, reading history, notes, and downloads stay on your device unless you explicitly share them.

## Download

Get the latest build from the **[releases page](https://github.com/jasontitus/kiwix-tauri/releases)**.

| Platform | Package |
|---|---|
| macOS 13.3+ (Apple silicon) | `.dmg` |
| Windows 10/11 on Intel or AMD (x64) | `Kiwix_…_x64-setup.exe` |
| Windows 11 on ARM (Snapdragon and other ARM64 PCs) | `Kiwix_…_arm64-setup.exe` (test build) |
| Ubuntu 24.04 (x86_64) | `.AppImage` with system WebKitGTK 4.1 |
| iPhone / iPad | Source build; sign with your own Apple developer account |
| Android (ARM64 / 32-bit ARM) | `.apk` |

Packages are development builds unless explicitly marked otherwise. Windows, macOS, and iOS packages are not App Store/notarized distribution builds; Android packages are development-signed. The development-signed iOS IPA is not published because it is tied to specific registered devices; build from source and sign with your own Apple developer account. The Windows installers are unsigned, so Windows SmartScreen asks you to confirm before running them. Check the release notes for the exact signing and runtime limits.

## What it does

Open a ZIM and Kiwix gives you a fast, quiet reader: a searchable library, tabs that survive a restart, readable typography, bookmarks and notes, offline maps, and downloads that can pause and resume. It is designed to work well on a small phone screen and on a large desktop without turning into two different applications.

## Feature tour

**Provenance icons** show where a feature was primarily inspired by an existing Kiwix reader. They are design cues, not code reuse, and a feature may exist in more than one upstream app.

| Icon | Reader |
|---|---|
| 🍎 | Kiwix for Apple (iOS / macOS) |
| 🤖 | Kiwix for Android |
| 🖥️ | Kiwix Desktop (Qt) |
| 🌐 | Kiwix JS (browser) |
| ✨ | Original to this project / shared across all platforms |

### Read and organize

| Feature | Inspired by |
|---|---|
| Open any ZIM book, including ones split into several files, where it is or as a copy in your library | 🖥️ 🍎 🤖 |
| A searchable library with covers, details, language and category filters, and sorting | 🖥️ 🍎 🤖 🌐 |
| Tabs that come back after a restart, back and forward buttons, and recent reading history | 🖥️ 🍎 🤖 |
| A clean article reader with working links, find in page, tables, references, pictures, audio and video | 🖥️ 🍎 🤖 🌐 |
| Zoom each book from 25% to 500%, and reset with one tap | 🖥️ 🍎 🤖 |
| Light, dark, and sepia reading themes | 🖥️ 🍎 🤖 |
| Bookmarks, a reading list, notes, tags, highlights, and comments on passages | 🖥️ 🍎 🤖 |
| Search titles and full text across all your books, with recent searches and a choice of which books to search | 🖥️ 🍎 🤖 🌐 |
| Hover or long-press a link to preview the article, with its picture | 🖥️ 🍎 🤖 |
| Save or open pictures, audio, video, and attachments | 🖥️ 🍎 🤖 |
| Save an article as a PDF, as a web page (HTML or single-file MHTML), or with all its files | 🖥️ 🍎 🤖 🌐 |
| Print articles on desktop | 🖥️ 🌐 |
| Read articles aloud with your device's voices, or with natural-sounding voices (a one-time download, English) on desktop and Android | 🍎 🤖 ✨ |
| Keyboard shortcuts, menus, and a compact reading bar on phones | 🖥️ 🍎 🤖 |

### Discover and download

| Feature | Inspired by |
|---|---|
| Browse online catalogs of books, search and filter them, and preview a book before downloading | 🖥️ 🍎 🤖 🌐 |
| Catalog pages you have seen stay browsable offline, and say when they may be out of date | 🖥️ 🌐 |
| Downloads you can pause, resume, cancel, and retry, with live progress | 🖥️ 🍎 🤖 |
| Download over the web or BitTorrent, whichever the book offers | 🖥️ 🤖 |
| Every download is checked for damage before it joins your library | 🖥️ 🍎 🤖 |
| Books split into several files download and open as one | 🖥️ 🍎 🤖 |
| Speed limits, a download queue, and a choice whether to keep sharing (seeding) finished torrents | 🖥️ 🤖 |
| Find newer editions of your books, replace them in one step, and roll back if you prefer the old one | ✨ |
| A notice when a download finishes, which opens the book without interrupting what you are reading | ✨ |

### Maps, sharing, and the local network

| Feature | Inspired by |
|---|---|
| Find offline street maps (StreetZim) by area and language | 🍎 ✨ |
| Build offline maps of the areas you choose, verified piece by piece, and keep them | 🍎 ✨ |
| Map downloads show their progress with your other downloads | 🍎 ✨ |
| **Nearby → Share all** lets another phone or computer on the same network pick books from yours | 🤖 ✨ |
| Nearby transfers are encrypted and need no PIN, port setup, or settings | 🤖 ✨ |
| Share your library to any web browser on your network, when you turn it on in Settings | 🖥️ 🌐 |
| Use BitTorrent or a direct transfer of selected books instead, if you prefer | 🖥️ 🤖 |
| Books you receive are not shared onward automatically, and sharing stops when the app restarts | ✨ |

### Study, accessibility, and language

| Feature | Inspired by |
|---|---|
| A reading list with notes, tags, and searchable history | 🖥️ 🍎 🤖 |
| Highlights and comments stay attached to the right passage, even after a restart | 🍎 🤖 |
| Export and import your reading list without losing anything (JSON or XML) | ✨ |
| Bring in your library and bookmarks from Kiwix Desktop or another Kiwix library | 🖥️ ✨ |
| The interface in 74 languages, with English filling any gaps | 🖥️ 🍎 🤖 🌐 |
| Right-to-left languages, and dates and numbers in your language's style | 🖥️ 🍎 🤖 |
| Screen-reader labels, visible focus, and full keyboard use (coverage varies by platform) | 🖥️ 🍎 🤖 |
| Larger text and layouts on phones that follow your system text size | 🍎 🤖 |

### Works with the rest of your computer

| Feature | Inspired by |
|---|---|
| **Look up in Kiwix**: run `kiwix-workbench --search "text"` or open a `kiwix://search?q=…` link from any app, script or bookmark, and Kiwix opens your library search with that text (desktop; the Windows installer, the macOS bundle and the Linux desktop entry register the `kiwix:` handler) | 🤖 ✨ |
| **AI assistant connector**: let AI assistants on your computer (Claude, VS Code, Cursor and other MCP apps) search and read your offline books. Off until you turn it on, read-only, and never in private mode (desktop) | ✨ |
| **Use your camera** when a book asks for a picture, such as StreetZim's photo search: choose the camera or a file (Windows; elsewhere the normal file picker opens) | ✨ |
| Drag and drop books onto the app, use Open With, share sheets, and your device's file pickers | 🖥️ 🍎 🤖 |
| Downloads keep going in the background where your phone allows it | 🍎 🤖 |
| Download notifications and Live Activities on supported phones | 🍎 🤖 |
| A headless mode with a private local interface, for scripts and automation | ✨ |
| Command-line options for a separate profile, private or read-only mode, and opening a specific article | 🖥️ ✨ |

### Privacy and your data

| Feature | Inspired by |
|---|---|
| Private mode: nothing you read or search is remembered | ✨ |
| Read-only mode: the app makes no changes to your files or library | ✨ |
| Each book runs in its own sealed space, so a book cannot see your other books or your data, and cannot use the camera or location unless you allow it | ✨ |
| Your data is saved safely, with backups and a clear message if anything is ever damaged | 🖥️ ✨ |
| Portable mode on desktop: settings, downloads, maps, and your library live beside the app, ready for a USB drive | 🖥️ ✨ |
| On macOS, the app runs sandboxed and only reads folders you grant it | 🍎 ✨ |

## Good to know

- **ZIM is the format.** Kiwix does not download individual web pages on demand. You need a ZIM archive, from the Kiwix catalog or another source.
- **Features vary by platform.** Desktop has print, portable mode, Look up, and the AI assistant connector; Windows also has the camera option for photo inputs. Mobile has share sheets, background transfers, and Live Activities. The tables above are a guide, not a promise that every control appears on every screen.
- **Windows on ARM is a test build.** It is built for ARM64 and checked file by file, but it has not yet been run on ARM hardware.
- **Some archives need platform web features.** Zimit2 archives are served through the shared reader path on desktop, iOS, and Android; real Zimit fixtures on packaged WKWebView/WebView2/WebKitGTK remain a release gate (see [Features](docs/FEATURES.md)). Older Zimit1 archives that require service-worker replay are not supported. Raw `.warc` files are not a Kiwix library format; `.warc` entries inside a ZIM can be saved with the reader's Save original file action. Archives that depend on specific codecs, WASM, or WebGL may still be limited by the system webview.
- **Maps need a network connection to build.** Once built, a saved map works offline.
- **Sharing is explicit.** Nearby starts only after you choose Share all. Sharing to web browsers is a separate opt-in and warns that it is unencrypted on the local network.
- **AI assistants see what they read.** When the connector is on, what an assistant searches and reads goes to that assistant, and to its provider if it uses a cloud model. See [Agent connector](docs/AGENT-CONNECTOR.md).
- **This is an independent project.** It is not affiliated with or endorsed by the Kiwix project. For the official apps, use the links above.

## Project documentation

| Document | What it covers |
|---|---|
| [Features and evidence](docs/FEATURES.md) | Detailed feature coverage, tested paths, and limits |
| [Platform comparison](docs/PLATFORM_COMPARISON.md) | How this implementation compares with the official readers |
| [Agent connector](docs/AGENT-CONNECTOR.md) | Setting up AI assistants with your offline library, and what they can see |
| [Platform integrations](docs/PLATFORM-INTEGRATIONS.md) | Design of the connector, Look up, on-device AI and Windows on ARM |
| [Architecture](docs/ARCHITECTURE.md) | Native services, security boundaries, and data flow |
| [Packaging](docs/PACKAGING.md) | SDK provenance, signing, AppImage and Debian packages, Windows ARM64, mobile and desktop builds |
| [Reliability](docs/RELIABILITY.md) | Runtime acceptance, failure handling, and known gaps |
| [Changelog](CHANGELOG.md) | Release history and verification notes |

## Build and contribute

Run these commands from the repository root with Node 22, stable Rust, a C++ toolchain and the platform’s Tauri development libraries. Cargo needs Git read access to the private archive libraries pinned in `src-tauri/Cargo.lock`. [Platform setup](docs/PACKAGING.md)

```sh
npm ci
npm run build
npm run desktop        # native Tauri app
npm run bundle:macos   # macOS app + dependency finalization
```

Linux AppImage and Debian packages, Windows (including ARM64), and mobile build commands are documented in [Packaging](docs/PACKAGING.md). See the packaging guide before publishing artifacts.

Changes must pass the independent review gate in [AGENTS.md](AGENTS.md). A successful compile is not a substitute for runtime evidence.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
