# Handy — cross-platform local speech-to-text desktop app (Tauri 2, Rust + React/TS)

Fork-specific guidance. `AGENTS.md` is left byte-identical to upstream so it never conflicts on merge — prefer this file, and only read `AGENTS.md` for upstream-authored detail.

## This checkout is a fork

- `origin` = `AlexBloch-IA/Handy` (Alex's fork), `upstream` = `cjpais/Handy`. Work branch: `feature/file-transcription`.
- Upstream is under a **feature freeze**: new features need community support in [Discussions](https://github.com/cjpais/Handy/discussions) before a PR. Fork work therefore stays local — do not open upstream PRs unprompted.
- Keep divergence rebasable: put new code in **new files**, touch as few existing lines as possible.

## Commands

Prereqs: Rust (stable) + Bun. Rust must be on PATH first: `. "$HOME/.cargo/env"`.

```bash
bun install
mkdir -p src-tauri/resources/models   # required once, dev won't run without the VAD model
curl -o src-tauri/resources/models/silero_vad_v4.onnx https://blob.handy.computer/silero_vad_v4.onnx

CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri dev     # full app (the env var avoids the macOS cmake error)
CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri build   # production bundle
bun run dev / build / preview                          # frontend only (Vite)

cargo test --manifest-path src-tauri/Cargo.toml        # Rust tests (CI runs `cargo test` from src-tauri/)
bun run test:playwright                                # E2E (tests/app.spec.ts)
bunx tsc --noEmit                                      # typecheck
bun run lint                                           # ESLint (src only)
bun run format / format:check                          # prettier + cargo fmt
bun run check:translations                             # CI gate — run after touching any locale file
```

CI (`code-quality.yml`) gates on `check:translations`, `lint`, `format:check`. Platform-specific build setup: `BUILD.md`.

## Architecture

- **Managers** (`src-tauri/src/managers/`: audio, model, transcription, history) are built at startup and held in Tauri state. Frontend → backend via Tauri commands, backend → frontend via events.
- **Dictation pipeline:** mic → VAD (Silero) → 16 kHz resample → Whisper (`transcribe-cpp`, GGML/GGUF) or ONNX models (`transcribe-rs`: Parakeet, Moonshine, SenseVoice) → clipboard/paste.
- **State flow:** Zustand (`src/stores/`) → Tauri command → Rust state → `tauri-plugin-store` persistence.
- **Single instance:** launching a second time raises the existing window. Remote-control CLI flags work by starting a second process that forwards its args through `tauri_plugin_single_instance` and exits; shared logic is `send_transcription_input()` in `signal_handle.rs`.
- **CLI flags** are defined in `cli.rs` (clap derive), parsed in `main.rs`, applied in `lib.rs`. They are runtime-only overrides and never mutate persisted settings. Note `--transcribe-file` exists upstream but accepts **WAV 16 kHz mono only**, with no progress output — that limitation is what the Fichiers tab replaces.
- Debug panel: `Cmd+Shift+D` (macOS) / `Ctrl+Shift+D`.

## Fichiers tab (fork feature, in progress)

Long-form audio transcription (30–60 min call recordings) via drag & drop, reusing the dictation engine untouched.

- Spec: `docs/superpowers/specs/2026-07-29-file-transcription-design.md` — read it before changing behaviour. Task-by-task plan: `docs/superpowers/plans/2026-07-29-file-transcription.md`. Both are in French.
- Core invariant: `TranscriptionManager::transcribe()` is **not** modified. A file chunk goes through the exact same call as a mic chunk, so dictation and file output can never diverge in quality.
- Backend `src-tauri/src/managers/file_transcription/` — `decode.rs` (symphonia → f32 mono), `chunk.rs` (pure `chunk_by_silence`, unit-testable without a model), `output.rs` (`.txt` next to the source, suffixed `-2` rather than ever overwriting), `mod.rs` (job types + worker thread). Commands in `commands/file_transcription.rs`, UI in `src/components/settings/files/`, store `src/stores/fileTranscriptionStore.ts`.
- Out of scope by design: LLM/summaries, diarization, timestamps/SRT, parallel jobs, any external binary (no ffmpeg). Jobs live in memory only — the `.txt` on disk is the sole durable artifact.

## Conventions

- **Never hand-edit `src/bindings.ts`** — tauri-specta regenerates it, and only in a debug build (`#[cfg(debug_assertions)]` in `lib.rs`). After adding a command, run `tauri dev` once, then use the exact generated (camelCase) names.
- **No literal strings in JSX** — ESLint (`eslint-plugin-i18next`) fails the build. Add the key to `src/i18n/locales/en/translation.json` (source of truth), then `t("key.path")`. Untranslated locales fall back to English; that is the accepted behaviour. Contributor rules: `CONTRIBUTING_TRANSLATIONS.md`.
- Rust: `anyhow` with context via `?`, no `unwrap` in production paths, `Arc<Mutex<T>>` for manager state.
- Conventional commits (`feat:`, `fix:`, `docs:`, `refactor:`, `chore:`), message explains *why*.

## Gotchas

- `tauri build` ends with a `TAURI_SIGNING_PRIVATE_KEY` error **after** producing the bundle. Expected — check for `Built application at:` / `Bundling Handy.app` above it before assuming failure.
- `cargo` not found in a fresh shell → source `$HOME/.cargo/env` first; every Rust command in the plan does.
- macOS cmake failure on `tauri dev` → prefix with `CMAKE_POLICY_VERSION_MINIMUM=3.5`.
- Linux overlay misbehaving under Wayland → `HANDY_NO_GTK_LAYER_SHELL=1` disables the GTK layer shell path (`overlay.rs`).
- symphonia's AAC/ALAC support is the weakest link for `.m4a` calls. macOS fallback: pre-convert with `/usr/bin/afconvert -f WAVE -d LEI16@16000 -c 1`.
- macOS needs accessibility permission for global shortcuts; acceleration is Metal (macOS) / Vulkan (Windows, Linux + OpenBLAS).

## Do not

- Do not open a PR or issue on `cjpais/Handy` without reading `.github/PULL_REQUEST_TEMPLATE.md` / `.github/ISSUE_TEMPLATE/` in full and filling every section, including the AI-assistance disclosure. Leave a TODO for "Human Written Description" — never write it in Alex's voice. Blank issues are disabled; feature requests belong in Discussions.
- Do not overwrite an existing transcript `.txt`, and do not make a failed disk write lose an in-memory transcript.

## graphify

This project has a knowledge graph at graphify-out/ with god nodes, community structure, and cross-file relationships.

Rules:
- For codebase questions, first run `graphify query "<question>"` when graphify-out/graph.json exists. Use `graphify path "<A>" "<B>"` for relationships and `graphify explain "<concept>"` for focused concepts. These return a scoped subgraph, usually much smaller than GRAPH_REPORT.md or raw grep output.
- If graphify-out/wiki/index.md exists, use it for broad navigation instead of raw source browsing.
- Read graphify-out/GRAPH_REPORT.md only for broad architecture review or when query/path/explain do not surface enough context.
- After modifying code, run `graphify update .` to keep the graph current (AST-only, no API cost).
