# AsySounds — audit initial (2026-09-24)

Goal: native Windows audio mixer and streaming routing, Rust core, Tauri/React UI later. No Python/Electron runtime.

Observed: SteelSeries Sonar virtual audio driver present; render endpoints Gaming, Chat, Media, Aux, Stream and Microphone; capture endpoints Stream and Microphone. HyperX Cloud III Wireless headphones and microphone, HyperX QuadCast S microphone, Shokz OpenRun Pro 2, Realtek, AMD monitor audio, webcam microphone. SteelSeries GG, Sonar, Engine, Moments, Prism running. OBS not running at audit time. Rust 1.98.1, Node 24.13.0; cl and cmake not found on PATH.

Safety: no uninstall, driver change, default endpoint change, service restart, or live audio interruption during initial development. Snapshot exact endpoint IDs/config before any routing migration. Test with explicit device selection and rollback. Windows virtual endpoints require a separately developed/signed driver; Microsoft SysVAD is a sample, not a ready-to-use real mixer.

Milestones: 1. DSP core + deterministic tests; 2. read-only Windows endpoint/session inventory and latency benchmark; 3. isolated WASAPI engine and device hotplug; 4. mixer/UI; 5. virtual driver and OBS integration only after validation.
