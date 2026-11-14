// AveeCare QuickBooks Connector window. Plain JS: every action is a Rust
// command (src-tauri/src/lib.rs); this file only shows state and words.
'use strict';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined && text !== null) n.textContent = text;
  return n;
};

const state = {
  info: null,
  profile: null,
  settings: null,
  history: [],
  running: false,
  quickbooks: null,
  pendingCode: null,
  lastPercent: 0,
  connectTimer: null,
};

// ---------- words ----------

const money = (cents) =>
  (cents / 100).toLocaleString('en-US', { style: 'currency', currency: 'USD' });

const hours = (minutes) => {
  const h = Math.round((minutes / 60) * 100) / 100;
  return `${h.toLocaleString('en-US')} ${h === 1 ? 'hour' : 'hours'}`;
};

const plural = (n, one, many) => `${n.toLocaleString('en-US')} ${n === 1 ? one : many}`;

function when(iso) {
  if (!iso) return '';
  const d = new Date(iso);
  const today = new Date();
  const yesterday = new Date(Date.now() - 86400000);
  const time = d.toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit' });
  if (d.toDateString() === today.toDateString()) return `Today at ${time}`;
  if (d.toDateString() === yesterday.toDateString()) return `Yesterday at ${time}`;
  return `${d.toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: d.getFullYear() === today.getFullYear() ? undefined : 'numeric' })} at ${time}`;
}

const fileName = (path) => (path || '').split(/[\\/]/).pop() || path || '';

function tone(status) {
  switch (status) {
    case 'completed': return 'ok';
    case 'nothingToSend': return 'ok';
    case 'completedWithProblems': return 'warn';
    case 'canceled': return 'idle';
    default: return 'bad';
  }
}

const glyphFor = { ok: '✓', warn: '!', bad: '!', idle: '–' };

function headline(run) {
  const refused = run.totals?.refused || 0;
  switch (run.status) {
    case 'completed': {
      const steps = run.steps || [];
      const landed = steps.some((s) => s.outcome === 'landed' && s.kind !== 'read');
      return !landed && steps.some((s) => s.outcome === 'already') ? 'Nothing needed changing' : 'Everything went in';
    }
    case 'completedWithProblems': return `${plural(refused, 'item needs', 'items need')} attention`;
    case 'nothingToSend': return 'Nothing was waiting to send';
    case 'canceled': return 'You stopped this sync';
    case 'stopped': return 'The sync stopped partway';
    default: return run.problem?.title || 'The sync could not start';
  }
}

function landedLine(run) {
  const t = run.totals || {};
  const parts = [];
  const hourSteps = (run.steps || []).filter((s) => s.kind === 'hours' && s.outcome === 'landed').length;
  if (hourSteps) parts.push(`${hours(t.hoursMinutesLanded || 0)} of pay`);
  if (t.invoicesLanded) parts.push(`${plural(t.invoicesLanded, 'invoice', 'invoices')} (${money(t.invoiceCentsLanded || 0)})`);
  if (t.paymentsLanded) parts.push(`${plural(t.paymentsLanded, 'payment', 'payments')} (${money(t.paymentCentsLanded || 0)})`);
  if (t.mileageChecksLanded) parts.push(`${plural(t.mileageChecksLanded, 'mileage check', 'mileage checks')} (${money(t.mileageCentsLanded || 0)})`);
  if (t.changesLanded) parts.push(`${plural(t.changesLanded, 'change', 'changes')} made in QuickBooks`);
  const alreadyChanges = (run.steps || []).filter((s) => s.outcome === 'already' && s.kind === 'change').length;
  const alreadyIn = Math.max(0, (t.already || 0) - alreadyChanges);
  if (alreadyIn) parts.push(`${plural(alreadyIn, 'item was', 'items were')} already in QuickBooks`);
  if (alreadyChanges) parts.push(`${plural(alreadyChanges, 'change was', 'changes were')} already done in QuickBooks`);
  return parts.join(' · ');
}

function subline(run) {
  const bits = [when(run.finishedAt || run.startedAt)];
  const landed = landedLine(run);
  if (landed) bits.push(landed);
  else if (run.status === 'stopped' || run.status === 'failed') bits.push(run.problem?.title || '');
  if (run.trigger === 'auto') bits.push('automatic');
  return bits.filter(Boolean).join(' · ');
}

// ---------- views ----------

function show(view) {
  for (const v of ['loading', 'updating', 'signin', 'app']) $(`view-${v}`).hidden = v !== view;
  state.view = view;
}

function showTab(tab) {
  for (const b of document.querySelectorAll('.segmented button')) b.classList.toggle('active', b.dataset.tab === tab);
  for (const t of ['sync', 'send', 'activity', 'settings']) $(`tab-${t}`).hidden = t !== tab;
  if (tab === 'activity') closeDetail();
}

function renderQuickBooks() {
  const qb = state.quickbooks || {};
  const last = state.history[0];
  const dot = $('qb-dot');
  const ready = qb.state === 'ready' || qb.state === 'emulator';
  dot.className = `dot ${ready ? 'ok' : 'warn'}`;
  if (ready) {
    $('qb-title').textContent = qb.state === 'emulator' ? 'QuickBooks emulator (testing)' : 'QuickBooks is open';
    const file = last?.companyFile ? fileName(last.companyFile) : '';
    const product = last?.productName || '';
    $('qb-sub').textContent = [file, product].filter(Boolean).join(' · ') || 'Ready to sync';
    $('qb-sub').title = last?.companyFile || '';
  } else {
    $('qb-title').textContent = qb.title || 'Checking QuickBooks…';
    $('qb-sub').textContent = '';
  }
  const fix = $('qb-fix');
  if (!ready && qb.fix && !state.running) {
    fix.hidden = false;
    fix.className = 'callout';
    $('qb-fix-text').textContent = qb.fix;
    $('qb-elevate').hidden = qb.state !== 'elevationMismatch';
  } else {
    fix.hidden = true;
  }
  $('sync-btn').disabled = state.running;
  $('sync-btn').textContent = state.running ? 'Syncing…' : 'Sync now';
}

function runRow(run, onOpen) {
  const li = document.querySelector('#run-row').content.firstElementChild.cloneNode(true);
  const t = tone(run.status);
  const icon = li.querySelector('.run-icon');
  icon.classList.add(t);
  icon.textContent = glyphFor[t];
  li.querySelector('.run-title').textContent = headline(run);
  li.querySelector('.run-sub').textContent = subline(run);
  li.addEventListener('click', () => onOpen(run));
  li.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      onOpen(run);
    }
  });
  return li;
}

function renderHistory() {
  const recent = $('recent-list');
  recent.replaceChildren(...state.history.slice(0, 5).map((r) => runRow(r, openRun)));
  $('recent-empty').hidden = state.history.length > 0;
  const all = $('activity-list');
  all.replaceChildren(...state.history.map((r) => runRow(r, openRun)));
  $('activity-empty').hidden = state.history.length > 0;
  $('clear-history').hidden = state.history.length === 0;
}

function openRun(run) {
  showTab('activity');
  $('activity-list-view').hidden = true;
  const detail = $('run-detail');
  detail.hidden = false;
  const back = el('button', 'btn quiet back', '‹ Activity');
  back.dataset.analyticsId = 'qbc-detail-back';
  back.addEventListener('click', closeDetail);
  detail.replaceChildren(back, resultCard(run, { full: true }));
}

function closeDetail() {
  $('run-detail').hidden = true;
  $('activity-list-view').hidden = false;
}

/** An undo/update AveeCare held because someone changed the record in QuickBooks by hand: QuickBooks refused nothing; the office decides in AveeCare. */
const isHeldChange = (s) => s.kind === 'change' && s.outcome === 'refused';

function itemNode(step) {
  const li = el('li', `item ${isHeldChange(step) ? 'held' : step.outcome}`);
  const glyph = isHeldChange(step) ? '!' : ({ landed: '✓', already: '✓', refused: '✕', stopped: '✕', read: '·' }[step.outcome] || '·');
  li.append(el('span', 'glyph', glyph));
  const body = el('div', 'item-body');
  body.append(el('div', null, step.title));
  if (step.detail) body.append(el('div', 'item-detail', step.detail));
  if (step.warning) body.append(el('div', 'item-warning', step.warning));
  if (step.fixPath) {
    const fix = el('button', 'link item-fix', step.fixLabel || 'Fix this in AveeCare');
    fix.dataset.analyticsId = 'qbc-open-fix';
    fix.addEventListener('click', () => invoke('open_in_aveecare', { path: step.fixPath }).catch(showToastError));
    body.append(fix);
  }
  li.append(body);
  return li;
}

function group(title, steps) {
  const frag = document.createDocumentFragment();
  if (!steps.length) return frag;
  frag.append(el('div', 'group-title', title));
  const ul = el('ul', 'items');
  for (const s of steps) ul.append(itemNode(s));
  frag.append(ul);
  return frag;
}

function problemActions(run) {
  const actions = el('div', 'result-actions');
  const code = run.problem?.code;
  if (code === 'wrongCompanyFile') {
    const use = el('button', 'btn', `Use ${fileName(run.problem.raw)}`);
    use.dataset.analyticsId = 'qbc-use-this-file';
    use.addEventListener('click', () => startSync(run.problem.raw));
    actions.append(use);
  }
  if (code === 'elevationMismatch') {
    const b = el('button', 'btn', 'Restart as administrator');
    b.dataset.analyticsId = 'qbc-restart-elevated-result';
    b.addEventListener('click', () => invoke('restart_elevated').catch(showToastError));
    actions.append(b);
  }
  if (code === 'SIGNED_OUT') {
    const b = el('button', 'btn', 'Sign in');
    b.dataset.analyticsId = 'qbc-sign-in-again';
    b.addEventListener('click', () => enterSignIn());
    actions.append(b);
  }
  // Syncing again cannot settle a change held for the office's decision; offer it only when something else went wrong.
  const onlyHeld = run.status === 'completedWithProblems' && (run.steps || []).filter((s) => s.outcome === 'refused' || s.outcome === 'stopped').every(isHeldChange);
  if (run.status !== 'completed' && run.status !== 'nothingToSend' && code !== 'wrongCompanyFile' && code !== 'SIGNED_OUT' && !onlyHeld) {
    const again = el('button', 'btn primary', 'Sync again');
    again.dataset.analyticsId = 'qbc-sync-again';
    again.addEventListener('click', () => startSync());
    actions.append(again);
  }
  return actions;
}

function resultCard(run, { full } = {}) {
  const card = el('div', 'card result-card');
  const head = el('div', 'result-head');
  const t = tone(run.status);
  const icon = el('span', `run-icon ${t}`, glyphFor[t]);
  const text = el('div');
  text.append(el('div', 'result-title', headline(run)));
  text.append(el('div', 'muted small', when(run.finishedAt || run.startedAt)));
  head.append(icon, text);
  card.append(head);

  if (run.problem && (run.status === 'stopped' || run.status === 'failed')) {
    const c = el('div', 'callout bad');
    if (run.status === 'stopped') c.append(el('p', null, run.problem.title));
    if (run.problem.fix) c.append(el('p', null, run.problem.fix));
    card.append(c);
  }

  const t2 = run.totals || {};
  const figures = el('div', 'figures');
  const fig = (num, lbl) => {
    const f = el('div', 'figure');
    f.append(el('div', 'num', num), el('div', 'lbl', lbl));
    figures.append(f);
  };
  const hourSteps = (run.steps || []).filter((s) => s.kind === 'hours' && s.outcome === 'landed').length;
  if (hourSteps) fig(hours(t2.hoursMinutesLanded || 0), `of pay on ${plural(hourSteps, 'timesheet line', 'timesheet lines')}`);
  if (t2.invoicesLanded) fig(money(t2.invoiceCentsLanded || 0), plural(t2.invoicesLanded, 'invoice', 'invoices'));
  if (t2.paymentsLanded) fig(money(t2.paymentCentsLanded || 0), plural(t2.paymentsLanded, 'payment', 'payments'));
  if (t2.mileageChecksLanded) fig(money(t2.mileageCentsLanded || 0), plural(t2.mileageChecksLanded, 'mileage check', 'mileage checks'));
  if (t2.changesLanded) fig(String(t2.changesLanded), t2.changesLanded === 1 ? 'change made in QuickBooks' : 'changes made in QuickBooks');
  const held = (run.steps || []).filter(isHeldChange).length;
  const refusedByQb = Math.max(0, (t2.refused || 0) - held);
  if (refusedByQb) fig(String(refusedByQb), 'refused by QuickBooks');
  if (held) fig(String(held), held === 1 ? 'change waiting for your decision' : 'changes waiting for your decision');
  if (figures.childElementCount) card.append(figures);

  const steps = run.steps || [];
  const problems = steps.filter((s) => s.outcome === 'refused' || s.outcome === 'stopped');
  const landed = steps.filter((s) => s.outcome === 'landed' && s.kind !== 'read');
  // An "already" undo or update (gone, or matching) is not a send QuickBooks
  // deduplicated, so the two are named apart.
  const alreadyDone = steps.filter((s) => s.outcome === 'already' && s.kind === 'change');
  const already = steps.filter((s) => s.outcome === 'already' && s.kind !== 'change');
  const reads = steps.filter((s) => s.outcome === 'read' || s.kind === 'read');
  card.append(group('Needs attention', problems));
  card.append(group('Landed in QuickBooks', full ? landed : landed.slice(0, 8)));
  // Shown even on the short card, so a run that changed nothing says why.
  card.append(group('Already done in QuickBooks', full ? alreadyDone : alreadyDone.slice(0, 8)));
  if (!full && landed.length > 8) {
    const more = el('button', 'link', `Show all ${landed.length}`);
    more.dataset.analyticsId = 'qbc-show-all-landed';
    more.addEventListener('click', () => openRun(run));
    card.append(more);
  }
  if (full) {
    card.append(group('Already in QuickBooks (nothing doubled)', already));
    card.append(group('Read from QuickBooks', reads));
    const meta = el('dl', 'meta');
    const row = (k, v) => {
      if (!v) return;
      meta.append(el('dt', null, k), el('dd', null, v));
    };
    row('Company file', run.companyFile);
    row('QuickBooks', run.productName);
    row('qbXML version', run.qbxmlVersion);
    row('Requests', run.requests ? String(run.requests) : '');
    row('Started by', run.trigger === 'auto' ? 'Automatic sync' : 'You');
    row('AveeCare said', run.closing);
    if (run.problem?.raw) row('QuickBooks said', run.problem.raw);
    card.append(meta);
  }
  card.append(problemActions(run));
  return card;
}

// ---------- sync ----------

function setProgress(label, percent) {
  $('progress').hidden = false;
  $('progress-label').textContent = label;
  const fill = $('bar-fill');
  if (percent === null) {
    fill.classList.add('indeterminate');
    fill.style.width = '';
  } else {
    fill.classList.remove('indeterminate');
    fill.style.width = `${Math.max(2, Math.min(100, percent))}%`;
  }
}

function liveCard() {
  let card = $('live-card');
  if (!card) {
    card = el('div', 'card');
    card.id = 'live-card';
    card.append(el('div', 'group-title', 'As it happens'));
    const ul = el('ul', 'items');
    ul.id = 'live-items';
    card.append(ul);
    $('result-area').replaceChildren(card);
  }
  return $('live-items');
}

async function startSync(allowFile) {
  if (state.running) return;
  // Show "connecting" BEFORE asking for the sync: a run that fails at once
  // (QuickBooks not open, a different company file) can finish, and its
  // result arrive, before invoke() returns. Setting this afterwards painted
  // "Connecting to QuickBooks…" over that result and never cleared it.
  state.running = true;
  state.lastPercent = 0;
  showTab('sync');
  $('result-area').replaceChildren();
  $('progress-hint').hidden = true;
  setProgress('Connecting to QuickBooks…', null);
  renderQuickBooks();
  try {
    await invoke('sync_now', { allowFile: allowFile || null });
  } catch (e) {
    state.running = false;
    $('progress').hidden = true;
    renderQuickBooks();
    showToastError(e);
  }
}

function onSyncEvent(e) {
  clearTimeout(state.connectTimer);
  // A sync started from the tray menu or the automatic timer.
  if (!state.running) {
    state.running = true;
    state.lastPercent = 0;
    $('result-area').replaceChildren();
    renderQuickBooks();
  }
  switch (e.phase) {
    case 'connectingToQuickBooks':
      setProgress('Connecting to QuickBooks…', null);
      // The first connection raises a window inside QuickBooks that a person
      // must answer; nothing moves until they do.
      state.connectTimer = setTimeout(() => {
        $('progress-hint').hidden = false;
        $('progress-hint').textContent =
          'Waiting for QuickBooks. If this is the first time, QuickBooks is showing a window asking whether to let "AveeCare Connector" in. Switch to QuickBooks, choose "Yes, always; allow access even if QuickBooks is not running", then Continue.';
      }, 8000);
      break;
    case 'quickBooksOpen':
      $('progress-hint').hidden = true;
      setProgress(`Connected to ${fileName(e.companyFile)}`, null);
      break;
    case 'askingAveeCare':
      setProgress('Checking AveeCare for anything waiting…', null);
      break;
    case 'started': {
      const p = e.plan || {};
      const parts = [];
      if (p.timesheetLines) parts.push(plural(p.timesheetLines, 'timesheet line', 'timesheet lines'));
      if (p.invoices) parts.push(plural(p.invoices, 'invoice', 'invoices'));
      if (p.payments) parts.push(plural(p.payments, 'payment', 'payments'));
      if (p.mileageChecks) parts.push(plural(p.mileageChecks, 'mileage check', 'mileage checks'));
      if (p.changes) parts.push(plural(p.changes, 'change', 'changes'));
      setProgress(parts.length ? `Sending ${parts.join(', ')}…` : 'Nothing waiting. Refreshing your QuickBooks lists for AveeCare…', 2);
      break;
    }
    case 'step':
      if (e.percent >= 0) state.lastPercent = e.percent;
      setProgress($('progress-label').textContent, state.lastPercent);
      if (e.result && e.result.outcome !== 'read') {
        const ul = liveCard();
        ul.prepend(itemNode(e.result));
        while (ul.childElementCount > 12) ul.lastElementChild.remove();
      }
      break;
    case 'finishing':
      setProgress('Finishing…', 100);
      break;
    default:
      break;
  }
}

async function onSyncFinished(run) {
  clearTimeout(state.connectTimer);
  state.running = false;
  $('progress').hidden = true;
  state.history = [run, ...state.history.filter((r) => r.id !== run.id)].slice(0, 100);
  if (run.problem?.code === 'SIGNED_OUT') {
    enterSignIn(run.problem.title);
    return;
  }
  $('result-area').replaceChildren(resultCard(run));
  renderHistory();
  await refreshQuickBooks();
}

// ---------- sign in ----------

function signinError(msg) {
  const p = $('signin-error');
  p.hidden = !msg;
  p.textContent = msg || '';
}

function enterSignIn(message) {
  state.profile = null;
  state.pendingCode = null;
  show('signin');
  $('signin-password-step').hidden = false;
  $('signin-code-step').hidden = true;
  $('signin-back').hidden = true;
  $('signin-submit').textContent = 'Sign in';
  $('signin-submit').disabled = false;
  $('signin-password').value = '';
  $('signin-email').value = state.settings?.lastEmail || '';
  $('signin-server').textContent = (state.info?.serverUrl || '').replace(/^https?:\/\//, '');
  signinError(message || '');
  ($('signin-email').value ? $('signin-password') : $('signin-email')).focus();
}

async function afterSignIn() {
  try {
    state.profile = await invoke('load_profile');
    await enterApp();
  } catch (e) {
    enterSignIn(e?.message || String(e));
  }
}

async function onSignInSubmit(ev) {
  ev.preventDefault();
  const btn = $('signin-submit');
  signinError('');
  btn.disabled = true;
  try {
    if (state.pendingCode) {
      btn.textContent = 'Checking…';
      await invoke('confirm_code', { ...state.pendingCode, code: $('signin-code').value });
      await afterSignIn();
      return;
    }
    const email = $('signin-email').value.trim();
    const password = $('signin-password').value;
    if (!email || !password) {
      signinError('Enter your email and password.');
      return;
    }
    btn.textContent = 'Signing in…';
    const r = await invoke('sign_in', { email, password });
    if (r.needsCode) {
      state.pendingCode = { session: r.session, username: r.username };
      $('signin-password-step').hidden = true;
      $('signin-code-step').hidden = false;
      $('signin-back').hidden = false;
      $('signin-code').value = '';
      $('signin-code').focus();
      return;
    }
    await afterSignIn();
  } catch (e) {
    signinError(e?.message || String(e));
  } finally {
    btn.disabled = false;
    btn.textContent = state.pendingCode ? 'Continue' : 'Sign in';
  }
}

// ---------- settings ----------

function renderSettings() {
  const s = state.settings || {};
  $('auto-sync').value = String(s.autoSyncMinutes || 0);
  $('start-with-windows').checked = !!s.startWithWindows;
  $('version').textContent = state.info?.version || '';
  $('server-url').value = state.info?.serverUrl || '';
  $('data-dir').textContent = state.info?.dataDir || '';
  const p = state.profile || {};
  const name = [p.firstName, p.lastName].filter(Boolean).join(' ');
  $('account-line').textContent = [name, p.email].filter(Boolean).join(' · ');
  $('agency-name').textContent = p.companyName || '';
}

let savedTimer = null;
async function saveSettings() {
  try {
    state.settings = await invoke('save_settings', {
      autoSyncMinutes: Number($('auto-sync').value),
      startWithWindows: $('start-with-windows').checked,
    });
    renderSettings();
    $('settings-saved').hidden = false;
    clearTimeout(savedTimer);
    savedTimer = setTimeout(() => ($('settings-saved').hidden = true), 1800);
  } catch (e) {
    showToastError(e);
    renderSettings();
  }
}

async function checkUpdate() {
  const btn = $('update-btn');
  const line = $('update-line');
  btn.disabled = true;
  try {
    if (btn.dataset.ready) {
      line.textContent = 'Downloading and installing… the Connector restarts by itself.';
      await invoke('install_update');
      return;
    }
    line.textContent = 'Checking…';
    const r = await invoke('check_update');
    if (r.available) {
      line.textContent = `Version ${r.version} is available.`;
      btn.textContent = 'Install and restart';
      btn.dataset.ready = '1';
    } else {
      line.textContent = 'You have the newest version.';
    }
  } catch (e) {
    line.textContent = e?.message || String(e);
  } finally {
    btn.disabled = false;
  }
}

function showToastError(e) {
  const msg = e?.message || String(e);
  const area = $('result-area');
  const c = el('div', 'card');
  const callout = el('div', 'callout bad');
  callout.append(el('p', null, msg));
  c.append(callout);
  if (!$('view-app').hidden) area.replaceChildren(c);
  else signinError(msg);
}

// ---------- boot ----------

async function refreshQuickBooks() {
  try {
    state.quickbooks = await invoke('quickbooks_status');
  } catch {
    state.quickbooks = null;
  }
  renderQuickBooks();
}

async function enterApp() {
  const s = await invoke('get_state');
  state.settings = s.settings;
  state.history = s.history || [];
  state.running = s.running;
  state.quickbooks = s.quickbooks;
  show('app');
  showTab('sync');
  renderSettings();
  renderHistory();
  renderQuickBooks();
  if (state.history[0] && !state.running) $('result-area').replaceChildren(resultCard(state.history[0]));
}

/**
 * MANDATORY UPDATE. Every launch asks GitHub for a newer version first and, if
 * there is one, installs it before the Connector does anything else, so no
 * agency keeps sending with an old build. Offline or GitHub unreachable: the
 * Connector opens as it is (sending payroll must not depend on GitHub) and
 * tries again every few hours. Never in the middle of a sync.
 */
const UPDATE_CHECK_TIMEOUT_MS = 15000;
const UPDATE_RECHECK_MS = 6 * 60 * 60 * 1000;

function withTimeout(promise, ms) {
  return Promise.race([promise, new Promise((_, reject) => setTimeout(() => reject(new Error('timed out')), ms))]);
}

async function installUpdateNow(version) {
  show('updating');
  if (version) $('updating-title').textContent = `Updating to version ${version}`;
  $('updating-error').hidden = true;
  for (const id of ['updating-retry', 'updating-skip', 'updating-skip-note']) $(id).hidden = true;
  $('updating-fill').style.width = '0%';
  try {
    await invoke('install_update');
  } catch (e) {
    $('updating-error').hidden = false;
    $('updating-error').textContent = e?.message || String(e);
    for (const id of ['updating-retry', 'updating-skip', 'updating-skip-note']) $(id).hidden = false;
    throw e;
  }
}

/** Resolves when the person chooses to open this version after a failed install. */
function waitForSkip() {
  return new Promise((resolve) => {
    $('updating-skip').onclick = () => resolve();
  });
}

/** True when an update was found and is installing (the app is about to close). */
async function updateIfAvailable() {
  let r;
  try {
    r = await withTimeout(invoke('check_update'), UPDATE_CHECK_TIMEOUT_MS);
  } catch (e) {
    console.warn('[update] check failed; opening as is', e);
    return false;
  }
  if (!r?.available) return false;
  const before = state.view;
  try {
    await installUpdateNow(r.version);
  } catch {
    // An update that will not install (a bad download, a release missing its
    // signature) must never lock an agency out of payroll: Try again stays on
    // screen beside a labeled way to open this version, and the update is
    // tried again on the next check and the next launch.
    await waitForSkip();
    if (before === 'app' || before === 'signin') show(before);
    return false;
  }
  return true;
}

async function boot() {
  await listen('update-progress', (e) => {
    const { downloaded, total } = e.payload || {};
    if (total) $('updating-fill').style.width = `${Math.min(100, Math.round((downloaded / total) * 100))}%`;
  });
  $('updating-retry').addEventListener('click', () => installUpdateNow().catch(() => {}));
  if (await updateIfAvailable()) return;
  setInterval(() => {
    if (!state.running) updateIfAvailable();
  }, UPDATE_RECHECK_MS);

  state.info = await invoke('app_info');
  const s = await invoke('get_state');
  state.settings = s.settings;
  if (!s.signedIn) {
    enterSignIn();
  } else {
    await afterSignIn();
  }

  $('signin-form').addEventListener('submit', onSignInSubmit);
  $('signin-back').addEventListener('click', () => enterSignIn());
  // Support only: another AveeCare server.
  $('signin-server').addEventListener('click', () => {
    $('signin-server-edit').hidden = false;
    $('signin-server-url').value = state.info.serverUrl;
    $('signin-server-url').focus();
  });
  $('signin-server-use').addEventListener('click', () => {
    invoke('set_server', { serverUrl: $('signin-server-url').value })
      .then(async () => {
        state.info = await invoke('app_info');
        $('signin-server-edit').hidden = true;
        enterSignIn();
      })
      .catch((e) => signinError(e?.message || String(e)));
  });
  for (const b of document.querySelectorAll('.segmented button')) b.addEventListener('click', () => showTab(b.dataset.tab));
  $('sync-btn').addEventListener('click', () => startSync());
  $('cancel-btn').addEventListener('click', () => {
    invoke('cancel_sync');
    $('progress-label').textContent = 'Stopping after this item…';
  });
  $('qb-elevate').addEventListener('click', () => invoke('restart_elevated').catch(showToastError));
  $('clear-history').addEventListener('click', async () => {
    await invoke('clear_history');
    state.history = [];
    renderHistory();
  });
  $('auto-sync').addEventListener('change', saveSettings);
  $('start-with-windows').addEventListener('change', saveSettings);
  $('update-btn').addEventListener('click', checkUpdate);
  $('signout-btn').addEventListener('click', async () => {
    try {
      await invoke('sign_out');
      enterSignIn();
    } catch (e) {
      showToastError(e);
    }
  });
  $('server-form').addEventListener('submit', async (ev) => {
    ev.preventDefault();
    $('server-error').hidden = true;
    try {
      await invoke('set_server', { serverUrl: $('server-url').value });
      state.info = await invoke('app_info');
      enterSignIn();
    } catch (e) {
      $('server-error').hidden = false;
      $('server-error').textContent = e?.message || String(e);
    }
  });

  await listen('sync-event', (e) => onSyncEvent(e.payload));
  await listen('sync-finished', (e) => onSyncFinished(e.payload));

  // QuickBooks opening or closing changes what the window should say.
  setInterval(() => {
    if (!$('view-app').hidden && !state.running) refreshQuickBooks();
  }, 5000);
}

boot().catch((e) => {
  show('signin');
  signinError(e?.message || String(e));
});
