import React, { useEffect, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { invoke } from '@tauri-apps/api/core';
import './style.css';

type View = 'Mixer' | 'Devices' | 'Microphone' | 'Settings';
type AudioSession = { id: string; pid: number; name: string; volume: number; muted: boolean; active: boolean };
type Devices = { inputs: string[]; outputs: string[]; default_input: string | null; default_output: string | null };
type VoiceSettings = { high_pass_hz: number; gate_threshold_db: number; compressor_threshold_db: number; compressor_ratio: number; makeup_db: number };
const defaultVoiceSettings: VoiceSettings = { high_pass_hz: 85, gate_threshold_db: -80, compressor_threshold_db: -20, compressor_ratio: 3, makeup_db: 3 };
const defaultNoiseStrength = 65;
const defaultImpactStrength = 85;
const defaultClarityStrength = 55;
const microphoneStorageKey = 'asysounds:microphone:v1';
const mixerStorageKey = 'asysounds:mixer:v1';
const defaultMixerLevels = [78, 65, 90, 72, 85];
const defaultMixerMuted = [false, false, false, false, false];
type SavedMicrophone = { input: string; output: string; noiseStrength: number; impactStrength: number; clarityStrength: number; voiceSettings: VoiceSettings };
type SavedMixer = { levels: number[]; muted: boolean[] };
function validControl(value: unknown, fallback: number, min: number, max: number): number {
  return typeof value === 'number' && Number.isFinite(value) ? Math.max(min, Math.min(max, value)) : fallback;
}
function readSavedMixer(): SavedMixer {
  try {
    const raw = window.localStorage.getItem(mixerStorageKey);
    if (!raw) return { levels: defaultMixerLevels, muted: defaultMixerMuted };
    const stored: unknown = JSON.parse(raw);
    if (typeof stored !== 'object' || stored === null || Array.isArray(stored)) return { levels: defaultMixerLevels, muted: defaultMixerMuted };
    const candidate = stored as Record<string, unknown>;
    const levels = Array.isArray(candidate.levels) && candidate.levels.length === defaultMixerLevels.length
      ? candidate.levels.map((value, index) => validControl(value, defaultMixerLevels[index], 0, 100))
      : defaultMixerLevels;
    const muted = Array.isArray(candidate.muted) && candidate.muted.length === defaultMixerMuted.length
      ? candidate.muted.map((value, index) => typeof value === 'boolean' ? value : defaultMixerMuted[index])
      : defaultMixerMuted;
    return { levels, muted };
  } catch {
    return { levels: defaultMixerLevels, muted: defaultMixerMuted };
  }
}

function readSavedMicrophone(): SavedMicrophone {
  const defaults: SavedMicrophone = { input: '', output: '', noiseStrength: defaultNoiseStrength, impactStrength: defaultImpactStrength, clarityStrength: defaultClarityStrength, voiceSettings: defaultVoiceSettings };
  try {
    const raw = window.localStorage.getItem(microphoneStorageKey);
    if (!raw) return defaults;
    const stored: unknown = JSON.parse(raw);
    if (typeof stored !== 'object' || stored === null || Array.isArray(stored)) return defaults;
    const candidate = stored as Record<string, unknown>;
    const voice = typeof candidate.voiceSettings === 'object' && candidate.voiceSettings !== null && !Array.isArray(candidate.voiceSettings)
      ? candidate.voiceSettings as Record<string, unknown> : {};
    return {
      input: typeof candidate.input === 'string' ? candidate.input : '',
      output: typeof candidate.output === 'string' ? candidate.output : '',
      noiseStrength: validControl(candidate.noiseStrength, defaultNoiseStrength, 0, 100),
      impactStrength: validControl(candidate.impactStrength, defaultImpactStrength, 0, 100),
      clarityStrength: validControl(candidate.clarityStrength, defaultClarityStrength, 0, 100),
      voiceSettings: {
        high_pass_hz: validControl(voice.high_pass_hz, defaultVoiceSettings.high_pass_hz, 20, 250),
        gate_threshold_db: validControl(voice.gate_threshold_db, defaultVoiceSettings.gate_threshold_db, -80, -20),
        compressor_threshold_db: validControl(voice.compressor_threshold_db, defaultVoiceSettings.compressor_threshold_db, -40, -6),
        compressor_ratio: validControl(voice.compressor_ratio, defaultVoiceSettings.compressor_ratio, 1, 10),
        makeup_db: validControl(voice.makeup_db, defaultVoiceSettings.makeup_db, -12, 12),
      },
    };
  } catch {
    // Corrupt or inaccessible WebView storage must not prevent startup.
    return defaults;
  }
}
// Strength controls the local RNNoise neural model, not the legacy gate threshold.
const strengthLabel = (strength: number) => strength === 0 ? 'Off' : strength < 34 ? 'Light' : strength < 70 ? 'Balanced' : strength < 86 ? 'Strong' : 'Maximum';
type Preview = { running: boolean; neural_enabled: boolean; inference_us: number; voice_probability: number; impact_events: number; diagnostic_remaining_ms: number; diagnostic_ready: boolean; peak: number; raw_peak: number; buffered_ms: number; overflow_samples: number; underflow_samples: number; device_xruns: number; failed: boolean; sample_rate: number; output_sample_rate: number; error: string | null };
type DiagnosticAudio = { original_wav: string; processed_wav: string };
const emptyPreview: Preview = { running: false, neural_enabled: false, inference_us: 0, voice_probability: 0, impact_events: 0, diagnostic_remaining_ms: 0, diagnostic_ready: false, peak: 0, raw_peak: 0, buffered_ms: 0, overflow_samples: 0, underflow_samples: 0, device_xruns: 0, failed: false, sample_rate: 0, output_sample_rate: 0, error: null };
const channelNames = ['Game', 'Chat', 'Media', 'Aux', 'Microphone'];
const channelIcons = ['🎮', '💬', '♫', '◈', '🎙'];

function App() {
  const [view, setView] = useState<View>('Microphone');
  const [savedMicrophone] = useState(readSavedMicrophone);
  const [savedMixer] = useState(readSavedMixer);
  const [levels, setLevels] = useState(savedMixer.levels);
  const [muted, setMuted] = useState<boolean[]>(savedMixer.muted);
  const [devices, setDevices] = useState<Devices>({ inputs: [], outputs: [], default_input: null, default_output: null });
  const [input, setInput] = useState(savedMicrophone.input);
  const [output, setOutput] = useState(savedMicrophone.output);
  const [preview, setPreview] = useState<Preview>(emptyPreview);
  const [voiceSettings, setVoiceSettings] = useState<VoiceSettings>(savedMicrophone.voiceSettings);
  const [noiseStrength, setNoiseStrength] = useState(savedMicrophone.noiseStrength);
  const [impactStrength, setImpactStrength] = useState(savedMicrophone.impactStrength);
  const [clarityStrength, setClarityStrength] = useState(savedMicrophone.clarityStrength);
  const [bypass, setBypass] = useState(false);
  // A/B comparison disables only the presence EQ, not RNNoise or impact suppression.
  const [clarityCompareOff, setClarityCompareOff] = useState(false);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const [diagnostic, setDiagnostic] = useState<DiagnosticAudio | null>(null);
  const [diagnosticBusy, setDiagnosticBusy] = useState(false);
  const [sessions, setSessions] = useState<AudioSession[]>([]);
  const [sessionsBusy, setSessionsBusy] = useState(false);
  const [sessionsError, setSessionsError] = useState('');
  const [editingSessionId, setEditingSessionId] = useState<string | null>(null);
  const editingSessionRef = useRef<string | null>(null);
  function editSession(id: string | null) { editingSessionRef.current = id; setEditingSessionId(id); }

  const defaultInput = devices.default_input && devices.inputs.includes(devices.default_input) ? devices.default_input : '';
  const defaultOutput = devices.default_output && devices.outputs.includes(devices.default_output) ? devices.default_output : '';
  const selectedInput = input || defaultInput;
  const selectedOutput = output || defaultOutput;

  async function refreshSessions() {
    try {
      const found = await invoke<AudioSession[]>('audio_sessions');
      if (!editingSessionRef.current) setSessions(found);
      setSessionsError('');
    } catch (error) { setSessionsError(String(error)); }
  }

  async function changeSession(id: string, update: { volume?: number; muted?: boolean }) {
    setSessionsBusy(true);
    try {
      await invoke('update_audio_session', { id, ...update });
      editSession(null);
      await refreshSessions();
    } catch (error) { editSession(null); setSessionsError(String(error)); await refreshSessions(); }
    finally { setSessionsBusy(false); }
  }

  async function refreshDevices() {
    try {
      const found = await invoke<Devices>('audio_devices');
      setDevices(found);
      setInput(current => found.inputs.includes(current) ? current : '');
      setOutput(current => found.outputs.includes(current) ? current : '');
      setMessage('');
    } catch (error) { setMessage(String(error)); }
  }

  useEffect(() => {
    try {
      window.localStorage.setItem(microphoneStorageKey, JSON.stringify({ input, output, voiceSettings, noiseStrength, impactStrength, clarityStrength }));
    } catch {
      // Preview remains available even if local persistence is blocked.
    }
  }, [input, output, voiceSettings, noiseStrength, impactStrength, clarityStrength]);

  useEffect(() => {
    try {
      window.localStorage.setItem(mixerStorageKey, JSON.stringify({ levels, muted }));
    } catch {
      // Mixer preview still works even if local persistence is blocked.
    }
  }, [levels, muted]);

  useEffect(() => {
    void refreshDevices();
    void invoke<Preview>('preview_status').then(setPreview).catch(() => {});
  }, []);
  useEffect(() => {
    if (view !== 'Mixer' || sessionsBusy || editingSessionId) return;
    void refreshSessions();
    const timer = window.setInterval(() => { if (!editingSessionRef.current) void refreshSessions(); }, 2500);
    return () => window.clearInterval(timer);
  }, [view, sessionsBusy, editingSessionId]);

  useEffect(() => {
    if (!preview.running) return;
    const timer = window.setInterval(async () => {
      try {
        const status = await invoke<Preview>('preview_status');
        setPreview(status);
        if (status.failed) setMessage(status.error ?? 'The audio device disconnected or its stream failed. Stop and refresh devices.');
      } catch (error) { setMessage(String(error)); }
    }, 120);
    return () => window.clearInterval(timer);
  }, [preview.running]);

  // Publish changes at most once per 90 ms; Rust applies them at a block boundary.
  useEffect(() => {
    if (!preview.running) return;
    const timer = window.setTimeout(() => {
      void invoke('update_preview_settings', { settings: voiceSettings, bypass, noiseStrength, impactStrength, clarityStrength: clarityCompareOff ? 0 : clarityStrength })
        .catch(error => setMessage(String(error)));
    }, 90);
    return () => window.clearTimeout(timer);
  }, [voiceSettings, bypass, noiseStrength, impactStrength, clarityStrength, clarityCompareOff, preview.running]);

  function changeVoiceSetting(key: keyof VoiceSettings, value: number) {
    setVoiceSettings(previous => ({ ...previous, [key]: value }));
  }

  function changeNoiseStrength(value: number) {
    setNoiseStrength(value);
    setBypass(false);
  }

  async function beginDiagnostic() {
    setDiagnosticBusy(true);
    try {
      await invoke('begin_diagnostic');
      setDiagnostic(null);
      setMessage('');
    } catch (error) { setMessage(String(error)); }
    finally { setDiagnosticBusy(false); }
  }

  async function loadDiagnostic() {
    setDiagnosticBusy(true);
    try {
      const clips = await invoke<DiagnosticAudio>('take_diagnostic');
      setDiagnostic(clips);
      // Stop headphone monitoring before the user plays either clip.
      await invoke('stop_preview');
      setPreview(emptyPreview);
      setMessage('');
    } catch (error) { setMessage(String(error)); }
    finally { setDiagnosticBusy(false); }
  }

  async function togglePreview() {
    setBusy(true);
    try {
      if (preview.running) {
        await invoke('stop_preview');
        setPreview(emptyPreview);
      } else {
        if (!selectedInput || !selectedOutput) { setMessage('Connect a microphone and headphone output, or choose devices manually.'); return; }
        await invoke('start_preview', { input: selectedInput, output: selectedOutput, settings: voiceSettings, bypass, noiseStrength, impactStrength, clarityStrength: clarityCompareOff ? 0 : clarityStrength });
        setPreview(await invoke<Preview>('preview_status'));
      }
      setMessage('');
    } catch (error) { setMessage(String(error)); }
    finally { setBusy(false); }
  }

  return <div className="app">
    <aside>
      <div className="logo"><span className="brand-mark">◉</span> <span className="brand-word">ASY<span>SOUNDS</span></span></div>
      <div className="sidebar-kicker">YOUR SOUND. YOUR RULES.</div>
      <nav aria-label="Main navigation">{(['Mixer', 'Devices', 'Microphone', 'Settings'] as View[]).map((name, index) =>
        <button key={name} className={'nav ' + (view === name ? 'active' : '')} onClick={() => setView(name)}>
          <span aria-hidden="true">{['◫', '◉', '⌁', '⚙'][index]}</span>{name}
        </button>)}</nav>
      <div className="asidefoot"><span className="footer-orb"/> ASYGAMES NETWORK<br/><small>AsySounds · Preview 0.1.0</small></div>
    </aside>
    <main>
      <header><div><div className="eyebrow">AUDIO CONTROL CENTER</div><h1>{view}</h1><p>{view === 'Microphone' ? 'Local noise, impact and voice clarity controls.' : 'Build your sound around your workflow.'}</p></div><span className={'status ' + (preview.running ? 'live-status' : '')}><span className="status-dot"/>{preview.running ? 'LIVE VOICE PREVIEW' : 'LOCAL AUDIO ENGINE'}</span></header>
      {message && <div role="alert" className="notice error">{message}</div>}
      {view === 'Microphone' && <section className="voice-panel simple-voice-panel">
        <div className="panel-heading"><div><span className="eyebrow">MICROPHONE STUDIO <span className="tiny-live-dot"/></span><h2>Clean voice, fewer distractions.</h2><p>Real-time neural processing on your PC. No cloud, no extra accounts.</p></div><button className="secondary" onClick={() => void refreshDevices()} disabled={preview.running}>↻ Refresh devices</button></div>
        <div className="device-grid compact-devices">
          <label>Microphone<select value={input} onChange={event => setInput(event.target.value)} disabled={preview.running}><option value="">{defaultInput ? 'Windows default · ' + defaultInput : 'No default microphone · choose device'}</option>{devices.inputs.map((name,index) => <option value={name} key={name + index}>{name}</option>)}</select></label>
          <label>Listen through<select value={output} onChange={event => setOutput(event.target.value)} disabled={preview.running}><option value="">{defaultOutput ? 'Windows default · ' + defaultOutput : 'No default output · choose device'}</option>{devices.outputs.map((name,index) => <option value={name} key={name + index}>{name}</option>)}</select></label>
          <div className="device-hint">Using Windows defaults only selects the current devices for this test. AsySounds does not change your system settings.</div>
        </div>
        <div className="simple-suppression">
          <div className="suppression-top"><div><span className="eyebrow">RNNOISE · LOCAL NEURAL PROCESSING</span><h3>Neural noise suppression</h3></div><div className="suppression-value"><strong>{noiseStrength}%</strong><small>{strengthLabel(noiseStrength)}</small></div></div>
          <input type="range" min="0" max="100" step="1" value={noiseStrength} aria-label="Noise reduction strength" onChange={event => changeNoiseStrength(Number(event.target.value))} style={{ background: `linear-gradient(90deg, #a58aff ${noiseStrength}%, #30364c ${noiseStrength}%)` }}/>
          <div className="suppression-labels"><span>Off</span><span>Balanced</span><span>Maximum</span></div>
          <p className="suppression-help">Mostly neural processing at the balanced setting. Use the impact filter below for short claps or keyboard strikes. Reduce intensity if syllables sound clipped.</p>
        </div>
        <div className="simple-preview">
          <div className="signal-head"><span className="eyebrow">LIVE SIGNAL</span><span className="signal-readout">{preview.running ? (Math.round(preview.voice_probability * 100) + '% voice detected') : 'Start a test to see input'}</span></div>
          <div className="simple-level"><span>Input</span><div className="meter-track"><div style={{width: (preview.running ? Math.min(100, preview.raw_peak * 100) : 0) + '%'}}/></div></div>
          <div className="simple-level"><span>Processed</span><div className="meter-track processed-track"><div style={{width: (preview.running ? Math.min(100, preview.peak * 100) : 0) + '%'}}/></div></div>
          <div className="signal-stats"><span><i className={preview.running && !preview.failed ? 'ok-dot' : 'idle-dot'}/>{preview.running ? (preview.failed ? 'Stream issue' : 'Processing locally') : 'Preview inactive'}</span><span>{preview.running ? 'RNNoise ' + preview.inference_us + ' µs / frame' : '48 kHz neural engine'}</span></div>
          <div className="simple-actions">
            <button className={preview.running ? 'stop' : 'primary'} disabled={busy} onClick={() => void togglePreview()}>{preview.running ? 'Stop listening' : 'Test microphone'}</button>
            <button className={'compare-button' + (bypass ? ' comparing' : '')} disabled={!preview.running || busy} aria-pressed={bypass} onClick={() => setBypass(current => !current)}>{bypass ? 'Original sound • ON' : 'Compare original sound'}</button>
          </div>
          <small className="simple-disclaimer">Use headphones to prevent feedback. If you still hear clicks when the preview is stopped, the sound comes from another monitor path (headset sidetone, Windows Listen or Sonar), not this filter.</small>
        </div>
        <section className="tuning-card diagnostic-card" aria-label="Record an original and processed comparison">
          <div className="tuning-head"><div><span className="eyebrow">REAL MICROPHONE CHECK</span><h3>Compare the same 5-second recording</h3></div><span className="prototype-tag">LOCAL ONLY</span></div>
          <small>Record only when you choose. Speak and make a click or clap; then stop the live preview before playing the two clips. Both use the exact same microphone input. Nothing is saved to disk or sent to a server.</small>
          <div className="simple-actions">
            <button className="secondary" disabled={!preview.running || preview.failed || bypass || busy || diagnosticBusy || preview.diagnostic_remaining_ms > 0 || preview.diagnostic_ready} onClick={() => void beginDiagnostic()}>{preview.diagnostic_remaining_ms > 0 ? 'Recording · ' + Math.ceil(preview.diagnostic_remaining_ms / 1000) + 's' : 'Record 5 seconds'}</button>
            <button className="compare-button" disabled={!preview.running || !preview.diagnostic_ready || busy || diagnosticBusy} onClick={() => void loadDiagnostic()}>{diagnosticBusy ? 'Preparing comparison…' : 'Finish & compare · stop preview'}</button>
          </div>
          {preview.diagnostic_remaining_ms > 0 && <small>Capturing the unprocessed and processed signals simultaneously…</small>}
          {preview.diagnostic_ready && <small>Recording ready. Select Finish & compare to stop headphone monitoring and play the two clips.</small>}
          {diagnostic && !preview.running && <div className="diagnostic-players">
            <label>Original microphone<audio controls preload="none" src={diagnostic.original_wav}/></label>
            <label>AsySounds processed<audio controls preload="none" src={diagnostic.processed_wav}/></label>
          </div>}
          <small>If both clips differ but you still hear the original sound live, check hardware sidetone, Windows “Listen to this device”, or Sonar monitoring. This is still a preview, not a virtual microphone for Discord/OBS.</small>
        </section>
        <section className="voice-tuning" aria-label="Voice cleanup and clarity">
          <div className="tuning-card"><div className="tuning-head"><div><span className="eyebrow">TRANSIENT CONTROL</span><h3>Clap & impact filter</h3></div><strong>{impactStrength}%</strong></div><input type="range" min="0" max="100" step="1" value={impactStrength} aria-label="Clap and impact suppression" onChange={event => { setImpactStrength(Number(event.target.value)); setBypass(false); }}/><small>Reduces isolated claps, clicks and short impacts. During speech it uses gentler reduction to protect words. {preview.running ? "Impacts detected: " + preview.impact_events : ""}</small></div>
          <div className="tuning-card"><div className="tuning-head"><div><span className="eyebrow">VOICE PRESENCE</span><h3>Voice clarity</h3></div><strong>{clarityStrength}%</strong></div><input type="range" min="0" max="100" step="1" value={clarityStrength} aria-label="Voice clarity" onChange={event => { setClarityStrength(Number(event.target.value)); setBypass(false); setClarityCompareOff(false); }}/><small>Vocal EQ: +{(6 * clarityStrength / 100).toFixed(1)} dB at 3 kHz, −{(3.5 * clarityStrength / 100).toFixed(1)} dB at 320 Hz. Compare ON/OFF on the same preview; Discord and OBS are not affected.</small><button type="button" className={"compare-button" + (clarityCompareOff ? " comparing" : "")} disabled={!preview.running || busy || bypass} aria-pressed={clarityCompareOff} onClick={() => setClarityCompareOff(value => !value)}>{clarityCompareOff ? "A/B: clarity OFF · tap for ON" : "A/B: clarity ON · tap for OFF"}</button></div>
        </section>
        <details className="advanced-panel">
          <summary>Advanced settings <span>Optional</span></summary>
          <div className="advanced-content">
            <p>Fine-tune only if you want to. These controls apply to the live preview.</p>
            <div className="controls-heading"><h3>Voice processing</h3><button className="secondary" onClick={() => {setVoiceSettings(defaultVoiceSettings); setNoiseStrength(defaultNoiseStrength); setImpactStrength(defaultImpactStrength); setClarityStrength(defaultClarityStrength); setClarityCompareOff(false); setBypass(false);}}>Reset settings</button></div>
            <div className="voice-controls">
              {([
                ['high_pass_hz', 'High-pass filter', 20, 250, 5, 'Hz'],
                ['gate_threshold_db', 'Noise gate threshold', -80, -20, 1, 'dB'],
                ['compressor_threshold_db', 'Compressor threshold', -40, -6, 1, 'dB'],
                ['compressor_ratio', 'Compression ratio', 1, 10, 0.5, ':1'],
                ['makeup_db', 'Makeup gain', -12, 12, 1, 'dB'],
              ] as const).map(([key,label,min,max,step,unit]) =>
                <label className="voice-control" key={key}><span>{label}<strong>{voiceSettings[key]}{unit === ':1' ? unit : ' ' + unit}</strong></span><input type="range" min={min} max={max} step={step} value={voiceSettings[key]} onChange={event => changeVoiceSetting(key, Number(event.target.value))} aria-label={label}/></label>)}
            </div>
            <label className="bypass-control"><input type="checkbox" checked={bypass} onChange={event => setBypass(event.target.checked)}/><span><strong>Bypass all effects</strong><small>Hear the dry microphone while preview is running.</small></span></label>
            {preview.running && <div className="telemetry"><span>Input {preview.sample_rate.toLocaleString()} Hz</span><span>Output {preview.output_sample_rate.toLocaleString()} Hz</span><span>Buffered: {preview.buffered_ms} ms</span><span>Overflow: {preview.overflow_samples.toLocaleString()}</span><span>Underflow: {preview.underflow_samples.toLocaleString()}</span><span>Device glitches: {preview.device_xruns.toLocaleString()}</span><span>RNNoise frame: {preview.inference_us} µs / 10,000 µs</span><span>Voice probability: {Math.round(preview.voice_probability * 100)}%</span></div>}
          </div>
        </details>
        <p className="limitation">RNNoise and the impact filter run locally at 48 kHz. Claps during speech may still be audible; this preview does not alter Discord, OBS or the Windows default microphone.</p>
      </section>}
      {view === 'Devices' && <section className="device-panel">
        <div className="panel-heading"><div><span className="eyebrow">CONNECTED AUDIO</span><h2>Your devices, clearly organized.</h2><p>Choose what AsySounds uses for microphone preview without changing Windows or Sonar.</p></div><button className="secondary" onClick={() => void refreshDevices()} disabled={preview.running}>↻ Refresh</button></div>
        <div className="device-overview">
          <div className="device-summary"><span className="device-symbol">♩</span><div><small>WINDOWS DEFAULT INPUT</small><strong>{defaultInput || 'Not available'}</strong><span>{devices.inputs.length} input device{devices.inputs.length === 1 ? '' : 's'} detected</span></div></div>
          <div className="device-summary"><span className="device-symbol">◉</span><div><small>WINDOWS DEFAULT OUTPUT</small><strong>{defaultOutput || 'Not available'}</strong><span>{devices.outputs.length} output device{devices.outputs.length === 1 ? '' : 's'} detected</span></div></div>
        </div>
        <div className="device-grid device-lists">
          <div><h3>Microphones <span className="device-count">{devices.inputs.length}</span></h3>
            {!devices.inputs.length && <p>No input devices were found. Check your Windows audio connections and refresh.</p>}
            {devices.inputs.map((name, index) => <div className={'device-row ' + (selectedInput === name ? 'selected-device' : '')} key={name + index}><div className="device-row-info"><strong>{name}</strong><div className="device-tags">{defaultInput === name && <span>WINDOWS DEFAULT</span>}{selectedInput === name && <span>PREVIEW INPUT</span>}</div></div><button className="device-action" disabled={preview.running || input === name} onClick={() => setInput(name)}>{input === name ? 'Selected' : 'Use for preview'}</button></div>)}
          </div>
          <div><h3>Playback <span className="device-count">{devices.outputs.length}</span></h3>
            {!devices.outputs.length && <p>No playback devices were found. Check your headphones and refresh.</p>}
            {devices.outputs.map((name, index) => <div className={'device-row ' + (selectedOutput === name ? 'selected-device' : '')} key={name + index}><div className="device-row-info"><strong>{name}</strong><div className="device-tags">{defaultOutput === name && <span>WINDOWS DEFAULT</span>}{selectedOutput === name && <span>PREVIEW OUTPUT</span>}</div></div><button className="device-action" disabled={preview.running || output === name} onClick={() => setOutput(name)}>{output === name ? 'Selected' : 'Use for preview'}</button></div>)}
          </div>
        </div>
        <div className="device-footer"><div><strong>Prefer automatic selection?</strong><span>Follow Windows defaults for the next preview; your Windows settings stay untouched.</span></div><button className="secondary" disabled={preview.running || (!input && !output)} onClick={() => {setInput(''); setOutput('');}}>Use Windows defaults</button></div>
      </section>}
      {view === 'Mixer' && <><div className="notice prototype-notice"><span className="prototype-tag">PREVIEW ONLY</span> Visual mixer prototype · channel levels and mute states are saved locally, but real per-app routing is not connected yet. The preview channels do not change Windows audio; the live session controls below do.</div><section className="mixer">{channelNames.map((name, index) => <article key={name}><div className="channelIcon">{channelIcons[index]}</div><h2>{name}</h2><div className="channel-meter"><div style={{ height: levels[index] + '%' }} /></div><input aria-label={name + ' volume preview'} type="range" min="0" max="100" value={levels[index]} onChange={event => setLevels(previous => previous.map((value, at) => at === index ? Number(event.target.value) : value))}/><strong>{levels[index]}%</strong><button className={muted[index] ? 'muted' : ''} onClick={() => setMuted(previous => previous.map((value, at) => at === index ? !value : value))}>{muted[index] ? 'Unmute' : 'Mute'}</button></article>)}</section></>}
      {view === 'Mixer' && <section className="device-panel session-panel">
        <div className="panel-heading"><div><span className="eyebrow">WINDOWS CORE AUDIO · DEFAULT PLAYBACK</span><h2>Live application sessions</h2><p>These controls change the actual Windows session volume on the current default playback device. They do not route apps to Game/Chat channels or change your default output.</p></div><button className="secondary" onClick={() => void refreshSessions()} disabled={sessionsBusy}>↻ Refresh sessions</button></div>
        {sessionsError && <div role="alert" className="notice error">{sessionsError}</div>}
        {!sessions.length && !sessionsError && <p>No playback sessions detected on the default output. Start audio in an application and refresh.</p>}
        <div className="session-list">{sessions.map(session => <div className="session-row" key={session.id}>
          <div className="session-meta"><strong>{session.name}</strong><small>PID {session.pid} · {session.active ? 'Active' : 'Inactive'} · Windows session</small></div>
          <label className="session-level"><span>Volume <strong>{Math.round(session.volume * 100)}%</strong></span><input type="range" min="0" max="100" step="1" value={Math.round(session.volume * 100)} disabled={sessionsBusy} aria-label={session.name + ' Windows session volume'} onFocus={() => editSession(session.id)} onPointerDown={() => editSession(session.id)} onChange={event => setSessions(previous => previous.map(item => item.id === session.id ? { ...item, volume: Number(event.target.value) / 100 } : item))} onPointerUp={event => void changeSession(session.id, { volume: Number(event.currentTarget.value) / 100 })} onPointerCancel={() => editSession(null)} onBlur={event => { editSession(null); void changeSession(session.id, { volume: Number(event.currentTarget.value) / 100 }); }} onKeyUp={event => { if (['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End', 'PageUp', 'PageDown'].includes(event.key)) void changeSession(session.id, { volume: Number(event.currentTarget.value) / 100 }); }}/></label>
          <button className={'device-action ' + (session.muted ? 'session-muted' : '')} disabled={sessionsBusy} onClick={() => void changeSession(session.id, { muted: !session.muted })}>{session.muted ? 'Unmute' : 'Mute'}</button>
        </div>)}</div>
        <small className="simple-disclaimer">Windows session volumes may also change in Volume Mixer or another application. This is real volume control, not independent virtual audio routing.</small>
      </section>}
      {view === 'Settings' && <section className="voice-panel"><span className="eyebrow">ENGINE STATUS</span><h2>Development build</h2><p>Local RNNoise suppression, adjustable live intensity, dry comparison and explicit-device preview are available. Persistent mixer routing, virtual channels and profiles are in development. Microphone preview preferences are saved locally.</p></section>}
    </main>
  </div>;
}

createRoot(document.getElementById('root')!).render(<App />);
