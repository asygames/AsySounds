# AsySounds

AsySounds is a Windows audio application under development. The current version is a prototype, **not** a replacement for SteelSeries Sonar yet. Its mixer controls are visual and do not change system audio. The Rust core has a tested mono voice processor and an opt-in live preview through selected devices. It has no virtual devices.

## Current components

- Rust DSP core: stereo gain/mix, output limiter, high-pass voice filter, hysteresis noise gate, compressor and makeup gain.
- Opt-in live voice preview CLI: `cargo run --bin voice_monitor -- --list` (or `--formats`), then `cargo run --bin voice_monitor -- "EXACT INPUT NAME" "EXACT OUTPUT NAME" 10`. Select headphones for output. Preview lasts at most 60 seconds and prints buffer overflow/underflow and device glitch counts. Common PCM formats and different input/output rates are converted in the preview; no virtual microphone is present.
- Read-only Windows endpoint inventory: `cargo run --bin devices`.
- Tauri/React UI: device list, live voice preview, adjustable high-pass/gate/compression/makeup, dry bypass, raw/processed peak meters and buffered/overflow/underflow/device-glitch telemetry. A single 0–100% noise-reduction slider maps to the gate threshold (0 disables the gate), with technical DSP controls preserved under a collapsed Advanced section. DSP controls update between native audio blocks without restarting the device; mixer remains visual. Run with `cd ui && npm run tauri dev`.
- Offline DSP throughput check: `cargo run --release --bin voice_bench`. This measures computation only, not device latency.

The voice gate suppresses sound between speech segments. It cannot separate speech from keyboard, music or other noise that occurs **during** speech. That requires a separately measured noise suppression model and a fallback path when it fails.

The preview reports device `Xrun` events separately from its own ring-buffer overflow/underflow. Live controls and bypass affect only the selected preview stream; they do not change the Windows microphone signal used by other apps. Settings are currently session-only (not persisted). A Bluetooth microphone may keep producing device glitches even when the resampler and app buffers stay healthy; the app must not silently call that stream stable.

## Engineering order

1. Measure the preview's conversion quality, latency and drift across real devices. Never change default devices without an explicit user action.
2. Measure end-to-end capture-to-output latency, glitches, CPU and memory over a long session. Handle device disconnect and sample-rate changes without losing settings.
3. Add persistent channels and per-app routing. Virtual microphones and independent game/chat/media endpoints require a signed Windows virtual audio driver and installer; UI sliders alone cannot provide them.
4. Add speech enhancement and noise suppression with listening tests, bypass, CPU limits and intelligible fallback. Add streaming presets and OBS integration after the basic engine is stable.
5. Run side-by-side measurements against Sonar on the same hardware before making performance or quality claims.

## Validation

Run `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cd ui && npm run build`, and `cd ui/src-tauri && cargo test`.

Development currently leaves Sonar, default endpoints, drivers and existing Windows audio settings untouched.
