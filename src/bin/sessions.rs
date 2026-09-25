fn main() {
    match asysounds_core::audio_sessions::list_audio_sessions() {
        Ok(sessions) => {
            println!(
                "Detected {} Windows playback sessions on the default endpoint",
                sessions.len()
            );
            for s in sessions {
                println!(
                    "PID {} | {} | {}% | {} | {}",
                    s.pid,
                    s.name,
                    (s.volume * 100.0).round() as u32,
                    if s.muted { "muted" } else { "audible" },
                    if s.active { "active" } else { "inactive" }
                );
            }
        }
        Err(e) => {
            eprintln!("Cannot enumerate Windows playback sessions: {e}");
            std::process::exit(1);
        }
    }
}
