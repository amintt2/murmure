const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
let snap = null;
let progress = {}; // id -> {downloaded,total}

const ICON_CHECK = '<svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 10.5l4 4 8-9"/></svg>';
const ICON_TRASH = '<svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M3.5 5.5h13M8 5.5V4a1 1 0 011-1h2a1 1 0 011 1v1.5M5 5.5l.8 10.1a1 1 0 001 .9h6.4a1 1 0 001-.9L15 5.5"/></svg>';

const isMac = navigator.platform.toLowerCase().includes('mac');
const modKey = isMac ? '⌘' : 'Ctrl';

function fmtBytes(b) {
  if (b > 1e9) return (b / 1e9).toFixed(2).replace('.', ',') + ' Go';
  if (b > 1e6) return Math.round(b / 1e6) + ' Mo';
  return Math.round(b / 1e3) + ' Ko';
}

function prettyShortcut(s) {
  return s.split('+').map((p) => {
    const k = p.trim().toLowerCase();
    if (isMac) {
      if (k === 'alt' || k === 'option') return '⌥';
      if (k === 'super' || k === 'cmd' || k === 'command' || k === 'meta') return '⌘';
      if (k === 'cmdorctrl' || k === 'commandorcontrol') return '⌘';
      if (k === 'ctrl' || k === 'control') return '⌃';
      if (k === 'shift') return '⇧';
    } else {
      if (k === 'super' || k === 'meta') return 'Win';
      if (k === 'cmdorctrl' || k === 'commandorcontrol') return 'Ctrl';
    }
    if (k === 'space') return isMac ? 'Espace' : 'Espace';
    return p.trim().length === 1 ? p.trim().toUpperCase() : p.trim();
  }).join(isMac ? ' ' : ' + ');
}

// ---------------------------------------------------------------------------
// Rendu
// ---------------------------------------------------------------------------

function renderStatus() {
  const st = $('status');
  const e = snap.engine;
  st.className = 'status ' + e.state;
  let text = e.message;
  const active = snap.models.find((m) => m.id === snap.settings.model_id);
  if (e.state === 'stopped' && active && !active.downloaded) text = 'Téléchargez le modèle pour commencer';
  if (e.state === 'stopped' && active && active.downloading) text = 'Téléchargement en cours…';
  if (snap.recording) text = 'Enregistrement…';
  $('status-text').textContent = text;

  $('foot-note').textContent = e.state === 'ready'
    ? `Appuyez sur ${prettyShortcut(snap.settings.shortcut)} dans n'importe quelle application.`
    : '';
  $('test').disabled = e.state !== 'ready';
  $('test').textContent = snap.recording ? 'Arrêter et transcrire' : 'Tester la dictée';
}

function runtimeOf(kind) { return snap.runtimes.find((r) => r.kind === kind); }

function renderModels() {
  const box = $('models');
  box.innerHTML = '';
  const groups = new Map();
  for (const m of snap.models) {
    if (!groups.has(m.engine)) groups.set(m.engine, []);
    groups.get(m.engine).push(m);
  }
  for (const [kind, list] of groups) {
    const rt = runtimeOf(kind);
    const head = document.createElement('div');
    head.className = 'group';
    head.innerHTML = `<span><b>${rt ? rt.label : kind}</b></span><span class="${rt && rt.found ? 'rt-ok' : 'rt-ko'}">${rt && rt.found ? 'moteur installé' : 'moteur absent'}</span>`;
    box.appendChild(head);
    for (const m of list) box.appendChild(modelRow(m, rt));
  }
}

function modelRow(m, rt) {
  const row = document.createElement('div');
  row.className = 'row model' + (m.id === snap.settings.model_id ? ' active' : '');
  row.innerHTML = `
    <div class="radio"></div>
    <div class="row-main">
      <div class="row-title">${m.name}</div>
      <div class="model-meta"><span>${m.params}</span><span>·</span><span>${m.size_label}</span><span>·</span><span>${m.languages}</span></div>
      <div class="model-desc">${m.description}</div>
    </div>
    <div class="model-right"></div>`;
  const right = row.querySelector('.model-right');
  if (m.downloading) {
    const p = progress[m.id];
    const pct = p && p.total ? Math.min(100, Math.round(p.downloaded / p.total * 100)) : 0;
    right.innerHTML = `<div class="progress"><i style="width:${pct}%"></i></div><span class="pct">${p && p.total ? pct + ' %' : '…'}</span>`;
  } else if (m.downloaded) {
    right.innerHTML = `<span class="check">${ICON_CHECK}Téléchargé</span><button class="icon-btn" title="Supprimer les fichiers">${ICON_TRASH}</button>`;
    right.querySelector('.icon-btn').onclick = async (ev) => {
      ev.stopPropagation();
      if (!confirm(`Supprimer les fichiers de ${m.name} ?`)) return;
      snap = await invoke('delete_model', { id: m.id });
      render();
    };
  } else {
    right.innerHTML = `<button class="btn">Télécharger</button>`;
    right.querySelector('.btn').onclick = async (ev) => {
      ev.stopPropagation();
      progress[m.id] = { downloaded: 0, total: 0 };
      await invoke('download_model', { id: m.id });
      await refresh();
    };
  }
  row.onclick = async () => {
    if (m.id === snap.settings.model_id) return;
    await save({ ...snap.settings, model_id: m.id });
  };
  return row;
}

function renderRuntimes() {
  const box = $('runtimes');
  box.innerHTML = '';
  for (const rt of snap.runtimes) {
    const row = document.createElement('div');
    row.className = 'row';
    const manual = snap.settings.runtime_paths[rt.id] || '';
    row.innerHTML = `
      <div class="row-main">
        <div class="row-title">${rt.label} <span class="row-sub" style="display:inline">· ${rt.binary}</span></div>
        <div class="row-sub rt-path" style="color:${rt.found ? '' : 'var(--danger)'}">${rt.found ? rt.found : 'Introuvable — ' + rt.install_hint}</div>
        <input class="input" type="text" placeholder="Chemin manuel (optionnel)" spellcheck="false" value="${manual.replace(/"/g, '&quot;')}" ${rt.found && !manual ? 'hidden' : ''}>
      </div>`;
    row.querySelector('.input').addEventListener('change', (e) => {
      const v = e.target.value.trim();
      const paths = { ...snap.settings.runtime_paths };
      if (v) paths[rt.id] = v; else delete paths[rt.id];
      save({ ...snap.settings, runtime_paths: paths });
    });
    box.appendChild(row);
  }
}

function renderSettings() {
  const s = snap.settings;
  $('shortcut').textContent = prettyShortcut(s.shortcut);
  $('shortcut-error').hidden = !snap.shortcut_error;
  $('shortcut-error').textContent = snap.shortcut_error || '';
  for (const b of $('mode').children) b.classList.toggle('on', b.dataset.v === s.mode);
  $('autopaste').checked = s.auto_paste;
}

function render() {
  renderStatus();
  renderModels();
  renderRuntimes();
  renderSettings();
}

async function refresh() {
  snap = await invoke('get_snapshot');
  render();
}

async function save(settings) {
  try {
    snap = await invoke('save_settings', { settings });
    render();
  } catch (e) {
    alert(e);
    await refresh();
  }
}

// ---------------------------------------------------------------------------
// Interactions
// ---------------------------------------------------------------------------

$('mode').addEventListener('click', (e) => {
  const v = e.target.dataset.v;
  if (v) save({ ...snap.settings, mode: v });
});
$('autopaste').addEventListener('change', (e) => save({ ...snap.settings, auto_paste: e.target.checked }));
$('restart').addEventListener('click', () => invoke('restart_engine'));
$('test').addEventListener('click', () => invoke('toggle_recording'));
$('open-log').addEventListener('click', (e) => { e.preventDefault(); invoke('open_engine_log'); });
$('open-models').addEventListener('click', (e) => { e.preventDefault(); invoke('open_models_dir'); });

// Capture du raccourci : clic, puis combinaison de touches.
const scBtn = $('shortcut');
let listening = false;
scBtn.addEventListener('click', () => {
  listening = true;
  scBtn.classList.add('listening');
  scBtn.textContent = 'Appuyez…';
  $('shortcut-help').textContent = 'Échap pour annuler';
});
function keyName(code) {
  if (code.startsWith('Key')) return code.slice(3);
  if (code.startsWith('Digit')) return code.slice(5);
  if (code.startsWith('Arrow')) return code.slice(5);
  if (code === 'Space' || /^F\d{1,2}$/.test(code)) return code;
  const ok = ['Enter', 'Tab', 'Backquote', 'Minus', 'Equal', 'Comma', 'Period', 'Slash', 'Semicolon', 'Quote', 'BracketLeft', 'BracketRight', 'Backslash', 'Home', 'End', 'PageUp', 'PageDown', 'Insert', 'Delete', 'Backspace'];
  return ok.includes(code) ? code : null;
}
window.addEventListener('keydown', (e) => {
  if (!listening) return;
  e.preventDefault();
  if (e.key === 'Escape') {
    listening = false; scBtn.classList.remove('listening'); renderSettings();
    $('shortcut-help').textContent = 'Cliquez puis appuyez sur les touches';
    return;
  }
  const mods = [];
  if (e.ctrlKey) mods.push('Ctrl');
  if (e.altKey) mods.push('Alt');
  if (e.shiftKey) mods.push('Shift');
  if (e.metaKey) mods.push('Super');
  const k = keyName(e.code);
  if (!k || ['Control', 'Alt', 'Shift', 'Meta'].includes(e.key)) return; // attendre la touche finale
  if (mods.length === 0) { $('shortcut-help').textContent = 'Ajoutez au moins un modificateur (⌥, ⌘, ⌃, ⇧)'; return; }
  listening = false; scBtn.classList.remove('listening');
  $('shortcut-help').textContent = 'Cliquez puis appuyez sur les touches';
  save({ ...snap.settings, shortcut: [...mods, k].join('+') });
});

// ---------------------------------------------------------------------------
// Événements du backend
// ---------------------------------------------------------------------------

listen('state-changed', refresh);
listen('runtime-progress', (e) => {
  const p = e.payload;
  const st = $('status');
  if (p.done) { refresh(); return; }
  st.className = 'status starting';
  const pct = p.total ? Math.round(p.downloaded / p.total * 100) : 0;
  $('status-text').textContent = `Installation du moteur ${p.kind}… ${pct} %`;
});
listen('model-progress', (e) => {
  const p = e.payload;
  if (p.error) { alert('Téléchargement échoué : ' + p.error); delete progress[p.id]; refresh(); return; }
  progress[p.id] = { downloaded: p.downloaded, total: p.total };
  if (p.done) { delete progress[p.id]; refresh(); return; }
  if (snap) {
    const row = [...document.querySelectorAll('.model')][snap.models.findIndex((m) => m.id === p.id)];
    if (row) {
      const pct = p.total ? Math.min(100, Math.round(p.downloaded / p.total * 100)) : 0;
      const bar = row.querySelector('.progress i'); const t = row.querySelector('.pct');
      if (bar) bar.style.width = pct + '%';
      if (t) t.textContent = p.total ? `${pct} % · ${fmtBytes(p.downloaded)}` : '…';
    }
  }
});

refresh();
