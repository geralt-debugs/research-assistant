# Research Assistant

A focused native research client powered by [Ollama Cloud](https://ollama.com), built with Tauri 2, React, Rust, and UniFFI.

On first launch the app asks for an Ollama API key (create one at [ollama.com/settings/keys](https://ollama.com/settings/keys)). The key is encrypted with a device-derived key (ChaCha20-Poly1305) and stored only in the platform app-config directory — it never leaves the device except to authenticate with `ollama.com`.

## Features

- Encrypted on-device API key storage
- Selectable Ollama Cloud chat models, refreshed live with capabilities
- Capability-aware image handling: questions with attached images are automatically routed to the main model when it supports vision, otherwise to a configured vision fallback (or the first vision-capable cloud model)
- Web research built on Ollama's `web_search` and `web_fetch` APIs: the model generates search queries, results are fetched and synthesised into a cited answer
- Speed, balanced, and quality research modes (context depth)
- Markdown answers, source cards, and native external links
- Follow-up questions with conversation context
- Device-local research history
- Responsive desktop and mobile layouts
- Shared Rust API client exported through UniFFI

## Architecture

- `src/`: React presentation layer and local research history
- `src-tauri/`: Tauri commands, encrypted settings storage, and platform configuration
- `crates/vane-core/`: Rust Ollama Cloud client, web search/fetch, research pipeline, and settings encryption

The Tauri shell links `vane-core` directly. The same public Rust records and `ResearchClient` object are exported with UniFFI for direct Swift or Kotlin use where a native integration needs to bypass the webview bridge.

## Development

Requirements:

- Node.js 20 or newer
- Rust stable
- [Tauri system dependencies](https://v2.tauri.app/start/prerequisites/)

```sh
npm install
npm run tauri dev
```

Build the current desktop platform:

```sh
npm run tauri build
```

## Mobile

Install the Tauri mobile prerequisites before initializing a platform. Android can be initialized on Linux, macOS, or Windows. iOS requires macOS and Xcode.

```sh
# Android development
npm run tauri android dev

# iOS, on macOS
npm run tauri ios init
npm run tauri ios dev
npm run tauri ios build
```

The application identifier is `com.rramaa.researchassistant`. Update it in `src-tauri/tauri.conf.json` before store submission if a different identifier is required.

### Android Release

The Android project is initialized in `src-tauri/gen/android`. Its release build reads the ignored signing credentials from `src-tauri/keys/keystore.properties` and signs with `src-tauri/keys/vane-release.jks`.

Build a signed universal APK for ARM64, ARMv7, x86, and x86_64:

```sh
npm run tauri android build -- --apk
```

The APK is written to:

```text
src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
```

Install it on a connected device:

```sh
adb install -r src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
```

Build a signed Android App Bundle for Google Play:

```sh
npm run tauri android build -- --aab
```

The keystore and credentials are intentionally not committed. Back up both files securely before publishing. See `src-tauri/keys/README.md` for signing details.

## UniFFI Bindings

Build the shared core and generate Kotlin bindings on Linux:

```sh
cargo build -p vane-core --release
cargo run -p vane-core --features bindgen --bin uniffi-bindgen -- \
  generate --library target/release/libvane_core.so \
  --language kotlin --out-dir bindings/kotlin
```

Generate Swift bindings on macOS by replacing the library path with `target/release/libvane_core.dylib` and `--language kotlin` with `--language swift`. The generated source must be packaged with a `vane-core` static or dynamic library built for each destination architecture.

## Verification

```sh
npm run build
cargo test -p vane-core
cargo check -p vane-research
cargo clippy --workspace --all-targets -- -D warnings
```
