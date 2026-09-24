import React, { useEffect, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { invoke } from '@tauri-apps/api/core';
import './style.css';

type View = 'Mixer' | 'Devices' | 'Microphone' | 'Settings';
type Devices = { inputs: string[]; outputs: string[] };
type Preview = { running: boolean; peak: number; overflow_samples: number; underflow_samples: number; device_xruns: number; failed: boolean; sample_rate: number; output_sample_rate: number; error: string | null };
const emptyPreview: Preview = { running: false, peak: 0, overflow_samples: 0, underflow_samples: 0, device_xruns: 0, failed: false, sample_rate: 0, output_sample_rate: 0, error: null };
const channelNames = ['Game', 'Chat', 'Media', 'Aux', 'Microphone'];
const channelIcons = ['🎮', '💬', '♫', '◈', '🎙'];

function App() {
  const [view, setView] = useState<View>('Microphone');
  const [levels, setLevels] = useState([78, 65, 90, 72, 85]);
  const [muted, setMuted] = useState<boolean[]>([false, false, false, false, false]);
  const [devices, setDevices] = useState<Devices>({ inputs: [], outputs: [] });
  const [input, setInput] = useState('');
  const [output, setOutput] = useState('');
  const [preview, setPreview] = useState<Preview>(emptyPreview);
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

  async function togglePreview() {
    setBusy(true);
    try {
      if (preview.running) {
        await invoke('stop_preview');
        setPreview(emptyPreview);
      } else {
        if (!input || !output) { setMessage('Select both a microphone and a headphone output.'); return; }
        await invoke('start_preview', { input, output });
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
      <header><div><div className="eyebrow">AUDIO CONTROL CENTER</div><h1>{view}</h1><p>{view === 'Microphone' ? 'Test your voice processing with selected devices.' : 'Build your sound around your workflow.'}</p></div><span className="status">● &nbsp; {preview.running ? 'Voice preview active' : 'Development preview'}</span></header>
      {message && <div role="alert" className="notice error">{message}</div>}
      {view === 'Microphone' && <section className="voice-panel">
        <div className="panel-heading"><div><span className="eyebrow">VOICE LAB</span><h2>Live voice preview</h2><p>High-pass filter, noise gate and compression run in the native audio engine.</p></div><button className="secondary" onClick={() => void refreshDevices()} disabled={preview.running}>Refresh devices</button></div>
        <div className="device-grid"><label>Microphone<select value={input} onChange={event => setInput(event.target.value)} disabled={preview.running}><option value="">Select input</option>{devices.inputs.map((name, index) => <option value={name} key={name + index}>{name}</option>)}</select></label><label>Listen through<select value={output} onChange={event => setOutput(event.target.value)} disabled={preview.running}><option value="">Select headphones</option>{devices.outputs.map((name, index) => <option value={name} key={name + index}>{name}</option>)}</select></label></div>
        <div className="voice-meter"><div className="meter-label"><span>Processed voice level</span><strong>{Math.round(preview.peak * 100)}%</strong></div><div className="meter-track"><div style={{ width: Math.min(100, preview.peak * 100) + '%' }} /></div></div>
        <div className="preview-actions"><button className={preview.running ? 'stop' : 'primary'} disabled={busy} onClick={() => void togglePreview()}>{preview.running ? 'Stop preview' : 'Start voice preview'}</button><span>Use headphones to avoid feedback. No Windows default device is changed.</span></div>
        {preview.running && <div className="telemetry"><span>Input {preview.sample_rate.toLocaleString()} Hz</span><span>Output {preview.output_sample_rate.toLocaleString()} Hz</span><span>Overflow: {preview.overflow_samples.toLocaleString()}</span><span>Underflow: {preview.underflow_samples.toLocaleString()}</span><span>Device glitches: {preview.device_xruns.toLocaleString()}</span></div>}
        <p className="limitation">The gate reduces noise between phrases. Noise that overlaps your voice needs a separate suppression model, which is not included yet.</p>
      </section>}
      {view === 'Devices' && <section className="device-panel"><div className="panel-heading"><div><span className="eyebrow">AVAILABLE HARDWARE</span><h2>Audio devices</h2></div><button className="secondary" onClick={() => void refreshDevices()}>Refresh</button></div><div className="device-grid"><div><h3>Inputs</h3>{devices.inputs.map((name, index) => <div className="device-row" key={name + index}>{name}</div>)}</div><div><h3>Outputs</h3>{devices.outputs.map((name, index) => <div className="device-row" key={name + index}>{name}</div>)}</div></div></section>}
      {view === 'Mixer' && <><div className="notice">Mixer controls are a visual prototype. They do not change Windows volume or Sonar routing yet.</div><section className="mixer">{channelNames.map((name, index) => <article key={name}><div className="channelIcon">{channelIcons[index]}</div><h2>{name}</h2><div className="channel-meter"><div style={{ height: levels[index] + '%' }} /></div><input aria-label={name + ' volume preview'} type="range" min="0" max="100" value={levels[index]} onChange={event => setLevels(previous => previous.map((value, at) => at === index ? Number(event.target.value) : value))}/><strong>{levels[index]}%</strong><button className={muted[index] ? 'muted' : ''} onClick={() => setMuted(previous => previous.map((value, at) => at === index ? !value : value))}>{muted[index] ? 'Unmute' : 'Mute'}</button></article>)}</section></>}
      {view === 'Settings' && <section className="voice-panel"><span className="eyebrow">ENGINE STATUS</span><h2>Development build</h2><p>Native voice processing and explicit-device preview are available. Persistent mixer routing, virtual channels, profiles and enhanced noise suppression are in development.</p></section>}
    </main>
  </div>;
}

createRoot(document.getElementById('root')!).render(<App />);
