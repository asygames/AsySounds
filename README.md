# AsySounds

AsySounds is a Windows audio application under development. The current version is a prototype, **not** a replacement for SteelSeries Sonar yet. Its five Game/Chat/Media/Aux/Microphone channel strips are visual only; the separate Windows playback-session controls change real application volume and mute on the current default playback device. The Rust core has a tested local RNNoise neural processor and an opt-in headphone preview through selected devices. It has no virtual devices.

## Current components

- Rust DSP core: stereo gain/mix, output limiter, high-pass voice filter, hysteresis noise gate, compressor and makeup gain.
- Opt-in live voice preview CLI: `cargo run --bin voice_monitor -- --list` (or `--formats`), then `cargo run --bin voice_monitor -- "EXACT INPUT NAME" "EXACT OUTPUT NAME" 10`. Select headphones for output. Preview lasts at most 60 seconds and prints buffer overflow/underflow and device glitch counts. Common PCM formats and different input/output rates are converted in the preview; no virtual microphone is present.
- Read-only Windows endpoint inventory: `cargo run --bin devices`.
- Native Windows Core Audio playback-session discovery and explicit per-session volume/mute controls on the current default output (without virtual routing or changing default devices). The session list uses application process names where possible and a background COM worker so polling does not block the WebView. Inspect sessions read-only via `cargo run --bin sessions`. The Mixer labels its five virtual channels as preview-only and lists real Windows sessions separately; modifying a real session also changes its volume in Windows Volume Mixer.
- Tauri/React UI: an animated dark studio, a simple 0–100% RNNoise slider and optional Advanced controls; selected-microphone/headphone preview with original-sound comparison. Devices displays Windows default input/output, marks the preview selection, and lets you explicitly choose hardware or follow the current Windows defaults without changing them. A dedicated worker resamples to 48 kHz and processes 480-sample (10 ms) neural frames. Audio callbacks only move samples through bounded buffers. Raw/processed levels, dropped samples, device glitches and neural frame time are reported. The five virtual mixer channels remain visual; the separate Windows session panel now uses real native volume/mute controls. Run with `cd ui && npm run tauri dev`.
- Offline DSP throughput checks: `cargo run --release --bin voice_bench` and `cargo run --release --bin noise_bench`. Synthetic offline measurements do not establish real-device latency or subjective audio quality.

The microphone preview additionally offers independently adjustable isolated clap/impact attenuation and gentle voice-presence EQ. Its RNNoise blend is now mostly wet at the balanced setting (rather than leaking nearly half of the dry signal). The transient detector operates on frame-aligned pre-filter audio; during speech, attenuation is deliberately limited to reduce lost syllables. This cannot guarantee removing a clap that overlaps the voice. Neither the filter nor clarity EQ changes the audio sent to Discord/OBS until a real processed microphone output is implemented. Controls are persistent; bypass is temporary. The responsive studio uses content-width-based reflow rather than fixed blank columns.

The older `voice_monitor` CLI retains the original gate/compressor pipeline. The Tauri preview now uses local neural suppression from `nnnoiseless` (RNNoise-derived, BSD-3-Clause). It can reduce overlapping background noise, but loud keyboard impacts, breathing or music may still pass, especially while speaking. A 100% setting is not a promise of complete silence.

The preview presents live input and processed signal levels, voice probability, and neural frame time; UI animations are cosmetic, not fabricated audio measurements. The preview reports device `Xrun` events separately from its own ring-buffer overflow/underflow. Live controls and bypass affect only the selected preview stream; they do not change the Windows microphone signal used by other apps. Microphone device choices, neural strength and Advanced voice controls are saved in the local app WebView storage and restored on launch (if devices are still available); bypass is deliberately session-only. Mixer sliders remain visual for now, but their channel levels and mute states are persisted locally between launches; they still do not change Windows or Sonar audio. A Bluetooth microphone may keep producing device glitches even when the resampler and app buffers stay healthy; the app must not silently call that stream stable.

## Engineering order

1. Measure the preview's conversion quality, latency and drift across real devices. Never change default devices without an explicit user action.
2. Measure end-to-end capture-to-output latency, glitches, CPU and memory over a long session. Handle device disconnect and sample-rate changes without losing settings.
3. Add persistent channels and per-app routing. Virtual microphones and independent game/chat/media endpoints require a signed Windows virtual audio driver and installer; UI sliders alone cannot provide them.
4. Validate RNNoise with voice, breathing and keyboard recordings, real-device latency and long-session CPU tests. Add a measured overload fallback, then streaming presets and OBS integration.
5. Run side-by-side measurements against Sonar on the same hardware before making performance or quality claims.

## Validation

Run `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cd ui && npm run build`, and `cd ui/src-tauri && cargo test`.

Development currently leaves Sonar, default endpoints, drivers and existing Windows audio settings untouched. RNNoise is local-only and requires no model API calls. See `docs/licenses/nnnoiseless-COPYING` for third-party attribution.

## License and permissions

Copyright (c) 2026 ASYGAMES. The original AsySounds code is **source-available, not open-source**, under [ASYGAMES SOURCE-AVAILABLE LICENSE v1.0](LICENSE). Viewing the public repository does not grant permission to modify, redistribute, sell or commercially exploit the original code without prior written authorization from the copyright holder. GitHub Terms of Service and mandatory statutory rights still apply. Third-party dependencies keep their own licenses; the RNNoise-derived `nnnoiseless` attribution is in [docs/licenses/nnnoiseless-COPYING](docs/licenses/nnnoiseless-COPYING).
