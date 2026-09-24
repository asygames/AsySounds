import React, { useEffect, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { invoke } from '@tauri-apps/api/core';
import './style.css';

type View = 'Mixer' | 'Devices' | 'Microphone' | 'Settings';
type Devices = { inputs: string[]; outputs: string[] };
type VoiceSettings = { high_pass_hz: number; gate_threshold_db: number; compressor_threshold_db: number; compressor_ratio: number; makeup_db: number };
const defaultVoiceSettings: VoiceSettings = { high_pass_hz: 85, gate_threshold_db: -80, compressor_threshold_db: -20, compressor_ratio: 3, makeup_db: 3 };
const defaultNoiseStrength = 55;
const microphoneStorageKey = 'asysounds:microphone:v1';
type SavedMicrophone = { input: string; output: string; noiseStrength: number; voiceSettings: VoiceSettings };
function validControl(value: unknown, fallback: number, min: number, max: number): number {
  return typeof value === 'number' && Number.isFinite(value) ? Math.max(min, Math.min(max, value)) : fallback;
}
function readSavedMicrophone(): SavedMicrophone {
  const defaults: SavedMicrophone = { input: '', output: '', noiseStrength: defaultNoiseStrength, voiceSettings: defaultVoiceSettings };
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
type Preview = { running: boolean; neural_enabled: boolean; inference_us: number; voice_probability: number; peak: number; raw_peak: number; buffered_ms: number; overflow_samples: number; underflow_samples: number; device_xruns: number; failed: boolean; sample_rate: number; output_sample_rate: number; error: string | null };
const emptyPreview: Preview = { running: false, neural_enabled: false, inference_us: 0, voice_probability: 0, peak: 0, raw_peak: 0, buffered_ms: 0, overflow_samples: 0, underflow_samples: 0, device_xruns: 0, failed: false, sample_rate: 0, output_sample_rate: 0, error: null };
const channelNames = ['Game', 'Chat', 'Media', 'Aux', 'Microphone'];
const channelIcons = ['🎮', '💬', '♫', '◈', '🎙'];

function App() {
  const [view, setView] = useState<View>('Microphone');
  const [savedMicrophone] = useState(readSavedMicrophone);
  const [levels, setLevels] = useState([78, 65, 90, 72, 85]);
  const [muted, setMuted] = useState<boolean[]>([false, false, false, false, false]);
  const [devices, setDevices] = useState<Devices>({ inputs: [], outputs: [] });
  const [input, setInput] = useState(savedMicrophone.input);
  const [output, setOutput] = useState(savedMicrophone.output);
  const [preview, setPreview] = useState<Preview>(emptyPreview);
  const [voiceSettings, setVoiceSettings] = useState<VoiceSettings>(savedMicrophone.voiceSettings);
  const [noiseStrength, setNoiseStrength] = useState(savedMicrophone.noiseStrength);
  const [bypass, setBypass] = useState(false);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');

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
      window.localStorage.setItem(microphoneStorageKey, JSON.stringify({ input, output, voiceSettings, noiseStrength }));
    } catch {
      // Preview remains available even if local persistence is blocked.
    }
  }, [input, output, voiceSettings, noiseStrength]);

  useEffect(() => {
    void refreshDevices();
    void invoke<Preview>('preview_status').then(setPreview).catch(() => {});
  }, []);
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
      void invoke('update_preview_settings', { settings: voiceSettings, bypass, noiseStrength })
        .catch(error => setMessage(String(error)));
    }, 90);
    return () => window.clearTimeout(timer);
  }, [voiceSettings, bypass, noiseStrength, preview.running]);

  function changeVoiceSetting(key: keyof VoiceSettings, value: number) {
    setVoiceSettings(previous => ({ ...previous, [key]: value }));
  }

  function changeNoiseStrength(value: number) {
    setNoiseStrength(value);
    setBypass(false);
  }

  async function togglePreview() {
    setBusy(true);
    try {
      if (preview.running) {
        await invoke('stop_preview');
        setPreview(emptyPreview);
      } else {
        if (!input || !output) { setMessage('Select both a microphone and a headphone output.'); return; }
        await invoke('start_preview', { input, output, settings: voiceSettings, bypass, noiseStrength });
        setPreview(await invoke<Preview>('preview_status'));
      }
      setMessage('');
    } catch (error) { setMessage(String(error)); }
    finally { setBusy(false); }
  }

  return <div className="app">
    <aside>
      <div className="logo"><span>◉</span> ASY<span>SOUNDS</span></div>
      <nav aria-label="Main navigation">{(['Mixer', 'Devices', 'Microphone', 'Settings'] as View[]).map((name, index) =>
        <button key={name} className={'nav ' + (view === name ? 'active' : '')} onClick={() => setView(name)}>
          <span aria-hidden="true">{['◫', '◉', '⌁', '⚙'][index]}</span>{name}
        </button>)}</nav>
      <div className="asidefoot">ASYGAMES NETWORK<br/><small>Development preview · 0.1.0</small></div>
    </aside>
    <main>
      <header><div><div className="eyebrow">AUDIO CONTROL CENTER</div><h1>{view}</h1><p>{view === 'Microphone' ? 'AI noise suppression with one simple control.' : 'Build your sound around your workflow.'}</p></div><span className="status">● &nbsp; {preview.running ? 'Voice preview active' : 'Development preview'}</span></header>
      {message && <div role="alert" className="notice error">{message}</div>}
      {view === 'Microphone' && <section className="voice-panel simple-voice-panel">
        <div className="panel-heading"><div><span className="eyebrow">MICROPHONE</span><h2>Cleaner voice. One slider.</h2><p>Choose the reduction level and listen to the result.</p></div><button className="secondary" onClick={() => void refreshDevices()} disabled={preview.running}>Refresh devices</button></div>
        <div className="device-grid compact-devices">
          <label>Microphone<select value={input} onChange={event => setInput(event.target.value)} disabled={preview.running}><option value="">Select microphone</option>{devices.inputs.map((name,index) => <option value={name} key={name + index}>{name}</option>)}</select></label>
          <label>Listen through<select value={output} onChange={event => setOutput(event.target.value)} disabled={preview.running}><option value="">Select headphones</option>{devices.outputs.map((name,index) => <option value={name} key={name + index}>{name}</option>)}</select></label>
        </div>
        <div className="simple-suppression">
          <div className="suppression-top"><div><span className="eyebrow">RNNOISE · LOCAL NEURAL PROCESSING</span><h3>Noise suppression strength</h3></div><div className="suppression-value"><strong>{noiseStrength}%</strong><small>{strengthLabel(noiseStrength)}</small></div></div>
          <input type="range" min="0" max="100" step="1" value={noiseStrength} aria-label="Noise reduction strength" onChange={event => changeNoiseStrength(Number(event.target.value))} style={{ background: `linear-gradient(90deg, #a58aff ${noiseStrength}%, #30364c ${noiseStrength}%)` }}/>
          <div className="suppression-labels"><span>Off</span><span>Balanced</span><span>Maximum</span></div>
          <p className="suppression-help">Start near 55%. Increase for keyboard, clicks and background sounds; reduce if your voice sounds unnatural.</p>
        </div>
        <div className="simple-preview">
          <div className="simple-level"><span>Voice level</span><div className="meter-track"><div style={{width: Math.min(100, preview.peak * 100) + '%'}}/></div></div>
          <div className="simple-actions">
            <button className={preview.running ? 'stop' : 'primary'} disabled={busy} onClick={() => void togglePreview()}>{preview.running ? 'Stop listening' : 'Test microphone'}</button>
            <button className={'compare-button' + (bypass ? ' comparing' : '')} disabled={!preview.running || busy} aria-pressed={bypass} onClick={() => setBypass(current => !current)}>{bypass ? 'Original sound • ON' : 'Compare original sound'}</button>
          </div>
          <small className="simple-disclaimer">Use headphones to prevent feedback. Windows audio defaults remain unchanged.</small>
        </div>
        <details className="advanced-panel">
          <summary>Advanced settings <span>Optional</span></summary>
          <div className="advanced-content">
            <p>Fine-tune only if you want to. These controls apply to the live preview.</p>
            <div className="controls-heading"><h3>Voice processing</h3><button className="secondary" onClick={() => {setVoiceSettings(defaultVoiceSettings); setNoiseStrength(defaultNoiseStrength); setBypass(false);}}>Reset settings</button></div>
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
        <p className="limitation">RNNoise processes audio locally at 48 kHz. Some loud impacts, breathing and typing during speech can still be audible; this preview does not alter Discord, OBS or the Windows default microphone.</p>
      </section>}
      {view === 'Devices' && <section className="device-panel"><div className="panel-heading"><div><span className="eyebrow">AVAILABLE HARDWARE</span><h2>Audio devices</h2></div><button className="secondary" onClick={() => void refreshDevices()}>Refresh</button></div><div className="device-grid"><div><h3>Inputs</h3>{devices.inputs.map((name, index) => <div className="device-row" key={name + index}>{name}</div>)}</div><div><h3>Outputs</h3>{devices.outputs.map((name, index) => <div className="device-row" key={name + index}>{name}</div>)}</div></div></section>}
      {view === 'Mixer' && <><div className="notice">Mixer controls are a visual prototype. They do not change Windows volume or Sonar routing yet.</div><section className="mixer">{channelNames.map((name, index) => <article key={name}><div className="channelIcon">{channelIcons[index]}</div><h2>{name}</h2><div className="channel-meter"><div style={{ height: levels[index] + '%' }} /></div><input aria-label={name + ' volume preview'} type="range" min="0" max="100" value={levels[index]} onChange={event => setLevels(previous => previous.map((value, at) => at === index ? Number(event.target.value) : value))}/><strong>{levels[index]}%</strong><button className={muted[index] ? 'muted' : ''} onClick={() => setMuted(previous => previous.map((value, at) => at === index ? !value : value))}>{muted[index] ? 'Unmute' : 'Mute'}</button></article>)}</section></>}
      {view === 'Settings' && <section className="voice-panel"><span className="eyebrow">ENGINE STATUS</span><h2>Development build</h2><p>Local RNNoise suppression, adjustable live intensity, dry comparison and explicit-device preview are available. Persistent mixer routing, virtual channels and profiles are in development. Microphone preview preferences are saved locally.</p></section>}
    </main>
  </div>;
}

createRoot(document.getElementById('root')!).render(<App />);
