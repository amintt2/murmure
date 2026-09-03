const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
let snap = null;
let progress = {}; // id -> {downloaded,total}
let filter = 'all';
let confirmId = null;

const isMac = navigator.platform.toLowerCase().includes('mac');
const ICON_CHECK = '<svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 10.5l4 4 8-9"/></svg>';
const ICON_TRASH = '<svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M3.5 5.5h13M8 5.5V4a1 1 0 011-1h2a1 1 0 011 1v1.5M5 5.5l.8 10.1a1 1 0 001 .9h6.4a1 1 0 001-.9L15 5.5"/></svg>';
const ENGINE_LABEL = { 'llama-cpp': 'llama.cpp', 'whisper-cpp': 'whisper.cpp', 'sherpa-onnx': 'sherpa-onnx' };

// ---------------------------------------------------------------------------
// Utilitaires
// ---------------------------------------------------------------------------

function esc(s) { return String(s ?? '').replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c])); }
function fmtBytes(b) {
  if (b > 1e9) return (b / 1e9).toFixed(2).replace('.', ',') + ' Go';
  if (b > 1e6) return Math.round(b / 1e6) + ' Mo';
  return Math.round(b / 1e3) + ' Ko';
}
function fmtNum(v, d = 1) { return v.toFixed(d).replace('.', ','); }
function sizeGo(label) {
  const m = /([\d,.]+)\s*(Go|Mo)/i.exec(label || '');
  if (!m) return null;
  const v = parseFloat(m[1].replace(',', '.'));
  return m[2].toLowerCase() === 'go' ? v : v / 1000;
}
// WER → score 0..1 (3 % = parfait, 12 % = nul)
function werScore(w) { return Math.max(0, Math.min(1, (12 - w) / 9)); }
function barClass(score) { return score > 0.68 ? 'g' : score > 0.4 ? 'a' : 'r'; }

let toastTimer = null;
function toast(msg) {
  const t = $('toast');
  t.textContent = msg; t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.hidden = true; }, 2800);
}

function prettyShortcut(s) {
  return s.split('+').map((p) => {
    const k = p.trim().toLowerCase();
    if (isMac) {
      if (k === 'alt' || k === 'option') return '⌥';
      if (['super', 'cmd', 'command', 'meta', 'cmdorctrl', 'commandorcontrol'].includes(k)) return '⌘';
      if (k === 'ctrl' || k === 'control') return '⌃';
      if (k === 'shift') return '⇧';
    } else {
      if (k === 'super' || k === 'meta') return 'Win';
      if (k === 'cmdorctrl' || k === 'commandorcontrol') return 'Ctrl';
    }
    if (k === 'space') return 'Espace';
    return p.trim().length === 1 ? p.trim().toUpperCase() : p.trim();
  }).join(isMac ? ' ' : ' + ');
}

// ---------------------------------------------------------------------------
// Métriques
// ---------------------------------------------------------------------------

function metric(label, wer, sub) {
  if (wer == null) return `<div class="metric na"><div class="metric-head"><span>${label}</span><b>non mesuré</b></div><div class="bar"></div></div>`;
  const s = werScore(wer);
  return `<div class="metric"><div class="metric-head"><span>${label}${sub ? ` <span>· ${sub}</span>` : ''}</span><b>${fmtNum(wer)} % WER</b></div><div class="bar"><i class="${barClass(s)}" style="width:${Math.round(s * 100)}%"></i></div></div>`;
}
function speedMetric(label, value, unit, max) {
  if (value == null) return `<div class="metric na"><div class="metric-head"><span>${label}</span><b>${unit === 'local' ? 'après une dictée' : 'non mesuré'}</b></div><div class="bar"></div></div>`;
  const s = Math.max(0.04, Math.min(1, Math.log10(value) / Math.log10(max)));
  return `<div class="metric"><div class="metric-head"><span>${label}</span><b>× ${value >= 100 ? Math.round(value) : fmtNum(value)} temps réel</b></div><div class="bar"><i class="g" style="width:${Math.round(s * 100)}%"></i></div></div>`;
}
function metricsHtml(m) {
  const b = m.bench || {};
  return `<div class="metrics">
    ${metric('Anglais', b.wer_en)}
    ${metric('Français', b.wer_fr, 'FLEURS')}
    ${speedMetric('Vitesse réf. (GPU A100)', b.rtfx, 'ref', 7000)}
    ${speedMetric('Vitesse sur cette machine', m.local_speed, 'local', 60)}
  </div>`;
}
function badges(m) {
  let h = `<span class="badge">${ENGINE_LABEL[m.engine] || m.engine}</span>`;
  if (m.recommended) h += '<span class="badge reco">Recommandé</span>';
  if (m.custom) h += '<span class="badge custom">Personnalisé</span>';
  return h;
}
function metaHtml(m) {
  return `<div class="meta"><span>${esc(m.params)} paramètres</span><span>·</span><span>${esc(m.size_label)}</span><span>·</span><span>${esc(m.languages)}</span><span>·</span><span>${esc(m.license)}</span></div>`;
}
function statusHtml(m) {
  if (m.downloading) {
    const p = progress[m.id];
    const pct = p && p.total ? Math.min(100, Math.round(p.downloaded / p.total * 100)) : 0;
    return `<span class="dl" data-id="${m.id}"><span class="progress"><i style="width:${pct}%"></i></span> <span class="pct">${p && p.total ? pct + ' %' : '…'}</span></span>`;
  }
  if (m.downloaded) return `<span class="check">${ICON_CHECK}Téléchargé</span>`;
  return `<span class="row-sub">Non téléchargé</span>`;
}

// ---------------------------------------------------------------------------
// Vue réglages
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
  $('foot-note').textContent = e.state === 'ready' ? `Appuyez sur ${prettyShortcut(snap.settings.shortcut)} dans n'importe quelle application.` : '';
  $('test').disabled = e.state !== 'ready';
  $('test').textContent = snap.recording ? 'Arrêter et transcrire' : 'Tester la dictée';
}

function renderActive() {
  const m = snap.models.find((x) => x.id === snap.settings.model_id);
  const box = $('active-model');
  if (!m) { box.innerHTML = '<div class="row"><div class="row-main">Aucun modèle sélectionné</div><button class="btn primary" id="choose">Choisir</button></div>'; $('choose').onclick = showPicker; return; }
  box.innerHTML = `<div class="active">
    <div class="active-head"><span class="active-name">${esc(m.name)}</span>${badges(m)}<button class="btn" id="choose">Changer</button></div>
    ${metaHtml(m)}
    ${metricsHtml(m)}
    <div class="mc-foot">${statusHtml(m)}<span class="spacer"></span>${m.downloaded || m.downloading ? '' : `<button class="btn primary" data-dl="${m.id}">Télécharger</button>`}</div>
  </div>`;
  $('choose').onclick = showPicker;
  box.querySelectorAll('[data-dl]').forEach((b) => b.onclick = () => download(b.dataset.dl));
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
        <div class="row-sub rt-path" style="color:${rt.found ? '' : 'var(--danger)'}">${esc(rt.found ? rt.found : 'Introuvable — ' + rt.install_hint)}</div>
        <input class="input" type="text" placeholder="Chemin manuel (optionnel)" spellcheck="false" value="${esc(manual)}" ${rt.found && !manual ? 'hidden' : ''}>
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
  $('perm-section').hidden = !isMac;
  $('perm-ok').hidden = !snap.accessibility;
  $('perm-btn').hidden = snap.accessibility;
  if (snap.accessibility) $('perm-ok').innerHTML = ICON_CHECK + 'Autorisé';
  $('shortcut').textContent = prettyShortcut(s.shortcut);
  $('shortcut-error').hidden = !snap.shortcut_error;
  $('shortcut-error').textContent = snap.shortcut_error || '';
  for (const b of $('mode').children) b.classList.toggle('on', b.dataset.v === s.mode);
  $('autopaste').checked = s.auto_paste;
}

// ---------------------------------------------------------------------------
// Vue modèles
// ---------------------------------------------------------------------------

function showPicker() { $('view-main').hidden = true; $('view-picker').hidden = false; renderPicker(); }
function showMain() { $('view-picker').hidden = true; $('view-main').hidden = false; $('add-form').hidden = true; }

function passes(m) {
  const b = m.bench || {};
  switch (filter) {
    case 'fr': return b.wer_fr != null && b.wer_fr <= 5.0;
    case 'fast': return (b.rtfx != null && b.rtfx >= 1500) || (m.local_speed != null && m.local_speed >= 15);
    case 'light': { const g = sizeGo(m.size_label); return g != null && g <= 1.2; }
    case 'downloaded': return m.downloaded;
    default: return true;
  }
}
function sorted(list) {
  return [...list].sort((a, b) => {
    if (a.custom !== b.custom) return a.custom ? 1 : -1;
    if (a.recommended !== b.recommended) return a.recommended ? -1 : 1;
    const wa = a.bench?.wer_en ?? 99, wb = b.bench?.wer_en ?? 99;
    return wa - wb;
  });
}

function renderPicker() {
  const box = $('picker-list');
  box.innerHTML = '';
  const list = sorted(snap.models.filter(passes));
  if (list.length === 0) { box.innerHTML = '<p class="legend">Aucun modèle ne correspond à ce filtre.</p>'; return; }
  for (const m of list) {
    const isActive = m.id === snap.settings.model_id;
    const card = document.createElement('div');
    card.className = 'model-card' + (isActive ? ' active' : '');
    const note = m.bench?.note ? `<div class="mc-note">${esc(m.bench.note)}</div>` : '';
    let actions = '';
    if (confirmId === m.id) {
      actions = `<span class="confirm">${m.custom ? 'Retirer le modèle et ses fichiers ?' : 'Supprimer les fichiers ?'} <button class="btn small danger" data-yes="${m.id}">Oui</button><button class="btn small" data-no="1">Non</button></span>`;
    } else {
      if (!isActive) actions += `<button class="btn ${m.downloaded ? 'primary' : ''}" data-use="${m.id}">Utiliser</button>`;
      else actions += `<span class="badge reco">Actif</span>`;
      if (!m.downloaded && !m.downloading) actions += `<button class="btn" data-dl="${m.id}">Télécharger</button>`;
      if (m.downloaded || m.custom) actions += `<button class="icon-btn" title="${m.custom ? 'Retirer ce modèle' : 'Supprimer les fichiers'}" data-del="${m.id}">${ICON_TRASH}</button>`;
    }
    card.innerHTML = `
      <div class="mc-head"><span class="mc-name">${esc(m.name)}</span>${badges(m)}</div>
      <div class="mc-desc">${esc(m.description)}</div>
      ${metaHtml(m)}
      ${metricsHtml(m)}
      ${note}
      <div class="mc-foot">${statusHtml(m)}<span class="spacer"></span>${actions}</div>`;
    box.appendChild(card);
  }
  box.querySelectorAll('[data-use]').forEach((b) => b.onclick = () => useModel(b.dataset.use));
  box.querySelectorAll('[data-dl]').forEach((b) => b.onclick = () => download(b.dataset.dl));
  box.querySelectorAll('[data-del]').forEach((b) => b.onclick = () => { confirmId = b.dataset.del; renderPicker(); });
  box.querySelectorAll('[data-no]').forEach((b) => b.onclick = () => { confirmId = null; renderPicker(); });
  box.querySelectorAll('[data-yes]').forEach((b) => b.onclick = () => removeModel(b.dataset.yes));
}

async function useModel(id) {
  await save({ ...snap.settings, model_id: id });
  const m = snap.models.find((x) => x.id === id);
  if (m && !m.downloaded && !m.downloading) download(id);
  renderPicker();
}
async function download(id) {
  progress[id] = { downloaded: 0, total: 0 };
  try { await invoke('download_model', { id }); } catch (e) { toast(String(e)); }
  await refresh();
}
async function removeModel(id) {
  confirmId = null;
  const m = snap.models.find((x) => x.id === id);
  try {
    snap = await invoke(m && m.custom ? 'remove_custom_model' : 'delete_model', { id });
    toast(m && m.custom ? 'Modèle retiré' : 'Fichiers supprimés');
  } catch (e) { toast(String(e)); await refresh(); }
  render();
}

// Formulaire d'ajout
function updateAddForm() {
  const eng = $('cm-engine').value;
  $('cm-mmproj-row').hidden = eng !== 'llama-cpp';
  $('cm-prompt-row').hidden = eng !== 'llama-cpp';
  $('cm-output-row').hidden = eng !== 'llama-cpp';
}
$('cm-engine').addEventListener('change', updateAddForm);
$('add-toggle').addEventListener('click', () => { $('add-form').hidden = !$('add-form').hidden; updateAddForm(); if (!$('add-form').hidden) $('cm-name').focus(); });
$('cm-cancel').addEventListener('click', () => { $('add-form').hidden = true; });
$('cm-save').addEventListener('click', async () => {
  const eng = $('cm-engine').value;
  const files = [$('cm-main').value.trim()];
  if (eng === 'llama-cpp' && $('cm-mmproj').value.trim()) files.push($('cm-mmproj').value.trim());
  const input = {
    name: $('cm-name').value.trim(),
    engine: eng,
    files,
    prompt: eng === 'llama-cpp' ? ($('cm-prompt').value.trim() || null) : null,
    output: eng === 'llama-cpp' ? $('cm-output').value : 'plain',
    languages: $('cm-langs').value.trim() || null,
  };
  try {
    snap = await invoke('add_custom_model', { input });
    $('add-form').hidden = true;
    for (const id of ['cm-name', 'cm-main', 'cm-mmproj', 'cm-prompt', 'cm-langs']) $(id).value = '';
    toast('Modèle ajouté. Téléchargez-le puis cliquez sur Utiliser.');
    render();
  } catch (e) { toast(String(e)); }
});

$('chips').addEventListener('click', (e) => {
  const f = e.target.dataset.f;
  if (!f) return;
  filter = f;
  for (const c of $('chips').children) c.classList.toggle('on', c.dataset.f === f);
  renderPicker();
});
$('back').addEventListener('click', showMain);

// ---------------------------------------------------------------------------
// Rendu global
// ---------------------------------------------------------------------------

function render() {
  renderStatus();
  renderActive();
  renderRuntimes();
  renderSettings();
  if (!$('view-picker').hidden) renderPicker();
}
async function refresh() { snap = await invoke('get_snapshot'); render(); }
async function save(settings) {
  try { snap = await invoke('save_settings', { settings }); render(); }
  catch (e) { toast(String(e)); await refresh(); }
}

// ---------------------------------------------------------------------------
// Interactions réglages
// ---------------------------------------------------------------------------

$('mode').addEventListener('click', (e) => { const v = e.target.dataset.v; if (v) save({ ...snap.settings, mode: v }); });
$('autopaste').addEventListener('change', (e) => save({ ...snap.settings, auto_paste: e.target.checked }));
$('restart').addEventListener('click', () => invoke('restart_engine'));
$('perm-btn').addEventListener('click', () => { invoke('request_accessibility'); setTimeout(refresh, 1500); });
window.addEventListener('focus', refresh);
$('test').addEventListener('click', () => invoke('toggle_recording'));
$('open-log').addEventListener('click', (e) => { e.preventDefault(); invoke('open_engine_log'); });
$('open-models').addEventListener('click', (e) => { e.preventDefault(); invoke('open_models_dir'); });

// Capture du raccourci
const scBtn = $('shortcut');
let listening = false;
scBtn.addEventListener('click', () => {
  listening = true; scBtn.classList.add('listening'); scBtn.textContent = 'Appuyez…';
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
  if (!k || ['Control', 'Alt', 'Shift', 'Meta'].includes(e.key)) return;
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
  if (p.done) { refresh(); return; }
  $('status').className = 'status starting';
  const pct = p.total ? Math.round(p.downloaded / p.total * 100) : 0;
  $('status-text').textContent = `Installation du moteur ${p.kind}… ${pct} %`;
});
listen('model-progress', (e) => {
  const p = e.payload;
  if (p.error) { toast('Téléchargement échoué : ' + p.error); delete progress[p.id]; refresh(); return; }
  progress[p.id] = { downloaded: p.downloaded, total: p.total };
  if (p.done) { delete progress[p.id]; refresh(); return; }
  const pct = p.total ? Math.min(100, Math.round(p.downloaded / p.total * 100)) : 0;
  document.querySelectorAll(`.dl[data-id="${p.id}"]`).forEach((el) => {
    const bar = el.querySelector('.progress i'); const t = el.querySelector('.pct');
    if (bar) bar.style.width = pct + '%';
    if (t) t.textContent = p.total ? `${pct} % · ${fmtBytes(p.downloaded)}` : '…';
  });
});

refresh();
