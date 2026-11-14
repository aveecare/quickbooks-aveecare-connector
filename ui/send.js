// Send tab: run payroll and billing from the Connector.
//
// Nothing here decides what is sent. Every number comes from the same AveeCare
// operations the web app's Payroll and Billing pages call (aveecare_call in
// src-tauri/src/lib.rs, aveecare.rs): the review is the server's dry run of
// the very send that follows, and the server applies every rule the web page
// does. This file only asks, shows the answer and passes the choice back.
'use strict';

const aveecare = (api, operation, input) => invoke('aveecare_call', { api, operation, input: input || {} });

const ymd = (d) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;

/** Last week, Sunday through Saturday: the first-time default. */
function lastWeek() {
  const now = new Date();
  const end = new Date(now.getFullYear(), now.getMonth(), now.getDate() - now.getDay() - 1);
  const start = new Date(end.getFullYear(), end.getMonth(), end.getDate() - 6);
  return { start: ymd(start), end: ymd(end) };
}

function rememberedRange(key) {
  try {
    const r = JSON.parse(localStorage.getItem(key) || 'null');
    if (r && /^\d{4}-\d{2}-\d{2}$/.test(r.start) && /^\d{4}-\d{2}-\d{2}$/.test(r.end)) return r;
  } catch { /* ignore */ }
  return lastWeek();
}

const shortDate = (s) => new Date(`${s}T12:00:00`).toLocaleDateString('en-US', { month: 'short', day: 'numeric' });

function readRange(prefix) {
  const start = $(`${prefix}-start`).value;
  const end = $(`${prefix}-end`).value;
  if (!start || !end) return { error: 'Pick both dates first.' };
  if (start > end) return { error: 'That range ends before it starts.' };
  return { start, end };
}

const errText = (e) => e?.message || String(e);

function callout(kind, text) {
  const c = el('div', `callout ${kind}`);
  if (Array.isArray(text)) {
    const ul = el('ul', 'plain-list');
    for (const t of text) ul.append(el('li', null, t));
    c.append(ul);
  } else {
    c.append(el('p', null, text));
  }
  return c;
}

function fixLink(label, path) {
  const b = el('button', 'link', label);
  b.type = 'button';
  b.addEventListener('click', () => invoke('open_in_aveecare', { path }).catch(showToastError));
  return b;
}

/** After queueing: sync now if QuickBooks can take it, or say how. */
async function syncAfterQueue(area, queuedSentence) {
  await refreshQuickBooks();
  const qb = state.quickbooks?.state;
  if (qb === 'ready' || qb === 'emulator') {
    showTab('sync');
    startSync();
    return;
  }
  area.replaceChildren(callout('ok', `${queuedSentence} ${state.quickbooks?.title || ''} Open QuickBooks, then press Sync now.`.trim()));
}

// ---------- payroll ----------

/** The web Payroll page's warnings, in short (app-pages/payroll.tsx describeQbDesktopBlockers). */
function payrollWarnings(check) {
  const out = [];
  const visits = (n) => plural(n, 'visit', 'visits');
  for (const c of (check.unmappedCaregivers || []).slice(0, 6)) out.push(`${c.name} is not matched to anyone in your QuickBooks employee list (${visits(c.visits)}).`);
  if ((check.unmappedCaregivers || []).length > 6) out.push(`…and ${check.unmappedCaregivers.length - 6} more caregivers who are not matched yet.`);
  for (const a of check.unmappedArchetypes || []) out.push(`No QuickBooks payroll item is set for ${a.archetype === 'PerVisit' ? 'per-visit / flat rate' : String(a.archetype).toLowerCase()} visits (${visits(a.visits)}).`);
  for (const r of check.unmappedRateCodes || []) out.push(`"${r.rateCode}" has no QuickBooks payroll item behind it (${visits(r.visits)}).`);
  for (const m of check.missingEmployeeRates || []) out.push(`${m.name} has no pay rate for "${m.item}" in QuickBooks, so these hours would be worth $0.00 (${visits(m.visits)}).`);
  for (const t of check.timeTrackingUnset || []) out.push(`QuickBooks will not accept hours for ${t.name} until "Use time data to create paychecks" is on in their Payroll Info (${visits(t.visits)}).`);
  for (const m of check.missingItems || []) out.push(`"${m.item}" is not a payroll item in your QuickBooks file (${visits(m.visits)}).`);
  if (check.orphanedVisits > 0) out.push(`${visits(check.orphanedVisits)} belong to caregivers no longer on your roster.`);
  if (check.zeroDuration > 0) out.push(`${visits(check.zeroDuration)} have no recorded time.`);
  for (const m of (check.rateMismatches || []).slice(0, 6)) out.push(`${m.name}, "${m.item}": AveeCare rate ${money(m.aveeCents)}, QuickBooks item rate ${money(m.qbCents)}.`);
  if (check.driveTimeVisitsUnmapped > 0) out.push(`${visits(check.driveTimeVisitsUnmapped)} have paid drive time but no QuickBooks payroll item for drive time, so that time will not be sent.`);
  for (const t of check.travelRatesMissing || []) out.push(`${t.name} has no rate on "${t.item}" in QuickBooks, so their drive time would arrive worth $0.00.`);
  return out;
}

const pay = { range: null, check: null, preview: null, includeAll: false, busy: false };

function paySelection() {
  const base = { periodStart: pay.range.start, periodEnd: pay.range.end };
  const c = pay.check;
  // Nothing held back: the period itself, exactly as the Payroll page sends it.
  if (!c || !(c.blocked > 0)) return base;
  const ids = pay.includeAll ? (c.allPayableVisitIds?.length ? c.allPayableVisitIds : c.readyVisitIds) : c.readyVisitIds;
  return { ...base, visitIds: ids || [] };
}

async function reviewPayroll() {
  const r = readRange('pay');
  const area = $('pay-review');
  if (r.error) { area.replaceChildren(callout('bad', r.error)); return; }
  pay.range = r;
  pay.includeAll = false;
  localStorage.setItem('qbc.payRange', JSON.stringify(r));
  area.replaceChildren(el('p', 'muted small', 'Checking with AveeCare…'));
  $('pay-check').disabled = true;
  try {
    pay.check = await aveecare('payroll', 'validateQuickBooksDesktopRun', { periodStart: r.start, periodEnd: r.end });
    await previewPayroll();
  } catch (e) {
    area.replaceChildren(callout('bad', errText(e)));
  } finally {
    $('pay-check').disabled = false;
  }
}

async function previewPayroll() {
  pay.preview = await aveecare('payroll', 'previewQuickBooksDesktopTimesheet', paySelection());
  renderPayroll();
}

function renderPayroll() {
  const area = $('pay-review');
  const c = pay.check || {};
  const p = pay.preview || { employees: [] };
  const employees = p.employees || [];
  const nodes = [];
  const worked = employees.reduce((n, e) => n + (e.workedMinutes || 0), 0);
  const lines = p.lineCount || 0;

  if (lines === 0) {
    nodes.push(callout('ok', `Nothing to send for ${shortDate(pay.range.start)} to ${shortDate(pay.range.end)}. Every payable visit in it is already in QuickBooks or waiting to go.`));
  } else {
    nodes.push(el('p', 'review-head', `${plural(employees.length, 'employee', 'employees')} · ${hours(worked)} · ${plural(lines, 'timesheet line', 'timesheet lines')}`));
    const table = el('table', 'review-table');
    const head = el('tr');
    for (const h of ['Employee', 'Hours', 'Overtime', 'Drive time']) head.append(el('th', null, h));
    table.append(head);
    for (const e of employees) {
      const tr = el('tr');
      tr.append(el('td', null, e.employeeName));
      tr.append(el('td', 'num', hours(e.workedMinutes || 0)));
      const ot = (e.overtimeMinutes || 0) + (e.doubleOvertimeMinutes || 0);
      tr.append(el('td', 'num', ot ? hours(ot) : '–'));
      tr.append(el('td', 'num', e.driveMinutes ? hours(e.driveMinutes) : '–'));
      table.append(tr);
    }
    nodes.push(table);
    if (employees.some((e) => (e.lines || []).some((l) => l.alreadyQueued))) {
      nodes.push(el('p', 'muted small', 'Includes hours already waiting from an earlier send; they go in the same sync.'));
    }
  }

  const warnings = payrollWarnings(c);
  if (c.blocked > 0) {
    nodes.push(el('p', 'review-sub', `${c.ready} of ${c.total} visits are ready; ${plural(c.blocked, 'visit needs', 'visits need')} attention`));
  }
  if (warnings.length) {
    const box = callout('warn', warnings);
    const actions = el('div', 'callout-actions');
    actions.append(fixLink('Fix in AveeCare', '/settings?setting=quickbooks'));
    box.append(actions);
    nodes.push(box);
  }
  if (c.blocked > 0) {
    const label = el('label', 'check-row');
    const box = el('input');
    box.type = 'checkbox';
    box.checked = pay.includeAll;
    box.dataset.analyticsId = 'qbc-pay-include-warned';
    box.addEventListener('change', async () => {
      pay.includeAll = box.checked;
      box.disabled = true;
      try { await previewPayroll(); } catch (e) { area.replaceChildren(callout('bad', errText(e))); }
    });
    label.append(box, el('span', null, `Also send the ${plural(c.blocked, 'visit', 'visits')} with warnings`));
    nodes.push(label);
  }

  if (lines > 0) {
    const row = el('div', 'send-actions');
    const send = el('button', 'btn primary', 'Send to QuickBooks');
    send.dataset.analyticsId = 'qbc-pay-send';
    send.addEventListener('click', () => sendPayroll(warnings, send));
    row.append(send);
    nodes.push(row);
  }
  area.replaceChildren(...nodes);
}

async function sendPayroll(warnings, btn) {
  if (pay.busy) return;
  pay.busy = true;
  btn.disabled = true;
  const area = $('pay-review');
  try {
    const sel = paySelection();
    const res = await aveecare('payroll', 'queueVisitsForQuickBooksDesktop', {
      ...sel,
      acknowledgedWarnings: warnings,
      choiceNote: pay.check?.blocked > 0
        ? (pay.includeAll ? `Sent all from the Connector despite ${pay.check.blocked} with warnings` : `Sent the ${pay.check.ready} ready from the Connector`)
        : 'Sent from the Connector',
    });
    const n = (res?.queued || 0) + (res?.blocksQueued || 0);
    pay.check = null;
    pay.preview = null;
    if (n === 0) {
      area.replaceChildren(callout('ok', 'Nothing new was queued: those hours are already in QuickBooks or waiting to go. Press Sync now to send anything waiting.'));
      return;
    }
    await syncAfterQueue(area, `${plural(n, 'item', 'items')} queued for QuickBooks.`);
  } catch (e) {
    area.replaceChildren(callout('bad', errText(e)));
  } finally {
    pay.busy = false;
    btn.disabled = false;
  }
}

// ---------- invoices ----------

const inv = { range: null, preview: null, created: null, validation: null, includeAll: false, busy: false };

async function reviewInvoices() {
  const r = readRange('inv');
  const area = $('inv-review');
  if (r.error) { area.replaceChildren(callout('bad', r.error)); return; }
  inv.range = r;
  inv.created = null;
  localStorage.setItem('qbc.invRange', JSON.stringify(r));
  area.replaceChildren(el('p', 'muted small', 'Checking with AveeCare…'));
  $('inv-check').disabled = true;
  try {
    // dryRun: reviewing never writes anything.
    inv.preview = await aveecare('billing', 'createInvoicesForRange', { startDate: r.start, endDate: r.end, dryRun: true });
    renderInvoicePreview();
  } catch (e) {
    area.replaceChildren(callout('bad', errText(e)));
  } finally {
    $('inv-check').disabled = false;
  }
}

const billable = (c) => !c.error && (c.visitCount || 0) > 0;

function renderInvoicePreview() {
  const area = $('inv-review');
  const p = inv.preview || { clients: [] };
  const clients = (p.clients || []).filter(billable);
  const nodes = [];
  if (!clients.length) {
    nodes.push(callout('ok', `No visits are ready to invoice from ${shortDate(inv.range.start)} to ${shortDate(inv.range.end)}.`));
  } else {
    const total = clients.reduce((n, c) => n + (c.totalCents || 0), 0);
    const visits = clients.reduce((n, c) => n + (c.visitCount || 0), 0);
    nodes.push(el('p', 'review-head', `${plural(clients.length, 'invoice', 'invoices')} · ${plural(visits, 'visit', 'visits')} · ${money(total)}`));
    const table = el('table', 'review-table');
    const head = el('tr');
    for (const h of ['Client', 'Visits', 'Amount']) head.append(el('th', null, h));
    table.append(head);
    for (const c of clients) {
      const tr = el('tr');
      tr.append(el('td', null, c.patientName));
      tr.append(el('td', 'num', String(c.visitCount)));
      tr.append(el('td', 'num', money(c.totalCents || 0)));
      table.append(tr);
    }
    nodes.push(table);
  }
  const w = p.warnings || {};
  const notes = [];
  if (w.noRateVisits) notes.push(`${plural(w.noRateVisits, 'visit has', 'visits have')} no bill rate and will not be invoiced.`);
  if (w.notCompletedVisits) notes.push(`${plural(w.notCompletedVisits, 'visit is', 'visits are')} not completed yet.`);
  if (w.notChargedVisits) notes.push(`${plural(w.notChargedVisits, 'visit is', 'visits are')} marked not to charge.`);
  for (const name of w.payerPlanClients || []) notes.push(`${name} is billed through a payer plan, not here.`);
  for (const c of (p.clients || []).filter((x) => x.error)) notes.push(`${c.patientName}: ${c.error}`);
  if (notes.length) nodes.push(callout('warn', notes));
  if (clients.length) {
    const row = el('div', 'send-actions');
    const go = el('button', 'btn primary', `Create ${plural(clients.length, 'invoice', 'invoices')}`);
    go.dataset.analyticsId = 'qbc-inv-create';
    go.addEventListener('click', () => createInvoices(clients, go));
    row.append(go, el('span', 'muted small', 'You review them before anything goes to QuickBooks.'));
    nodes.push(row);
  }
  area.replaceChildren(...nodes);
}

async function createInvoices(clients, btn) {
  if (inv.busy) return;
  inv.busy = true;
  btn.disabled = true;
  const area = $('inv-review');
  const progress = el('p', 'muted small', '');
  area.replaceChildren(progress);
  // One client per request (AppSync gives up at 30 seconds), one run id for
  // the lot so the run groups together, and can be undone, in AveeCare.
  const runId = crypto.randomUUID();
  const made = [];
  const failed = [];
  try {
    for (let i = 0; i < clients.length; i += 1) {
      const c = clients[i];
      progress.textContent = `Creating invoices… ${i + 1} of ${clients.length}`;
      try {
        const part = await aveecare('billing', 'createInvoicesForRange', {
          startDate: inv.range.start, endDate: inv.range.end, dryRun: false, patientIds: [c.patientId], runId,
        });
        for (const r of part?.clients || []) {
          if (r.invoiceId) made.push(r);
          else if (r.error) failed.push(`${r.patientName}: ${r.error}`);
        }
      } catch (e) {
        const m = errText(e);
        if (/may still have done this|did not answer in time/i.test(m)) {
          failed.push(`${c.patientName}: AveeCare did not answer in time. The invoice may exist; check Billing before running again (a visit is never billed twice).`);
        } else {
          failed.push(`${c.patientName}: ${m}`);
        }
      }
    }
    inv.created = { made, failed };
    if (!made.length) {
      area.replaceChildren(callout('bad', failed.length ? failed : ['No invoices were created.']));
      return;
    }
    inv.includeAll = false;
    inv.validation = await aveecare('billing', 'qbd-validate-invoices', { invoiceIds: made.map((m) => m.invoiceId) });
    renderInvoiceSend();
  } catch (e) {
    area.replaceChildren(callout('bad', errText(e)));
  } finally {
    inv.busy = false;
  }
}

function invoiceWarnings(v) {
  const out = [];
  if (v.serviceItemMissing) out.push('QuickBooks needs a service item before it can take an invoice; without one it refuses every line. Pick one in AveeCare under Settings → QuickBooks Desktop.');
  for (const b of v.blockedClients || []) out.push(`${b.name}: ${b.reason}`);
  if (v.unmappedRateCodes > 0) out.push(`${plural(v.unmappedRateCodes, 'rate code has', 'rate codes have')} no QuickBooks item of its own and will book to "${v.serviceItemName || 'the default item'}"${v.unmappedRateCodeNames?.length ? `: ${v.unmappedRateCodeNames.slice(0, 5).join(', ')}` : ''}.`);
  for (const n of v.newCustomers || []) {
    out.push(n.similarTo?.length
      ? `${n.name} will be created as a new QuickBooks customer. Similar names already there: ${n.similarTo.join(', ')}.`
      : `${n.name} will be created as a new QuickBooks customer.`);
  }
  return out;
}

function renderInvoiceSend() {
  const area = $('inv-review');
  const { made, failed } = inv.created;
  const v = inv.validation || {};
  const total = made.reduce((n, m) => n + (m.totalCents || 0), 0);
  const nodes = [callout('ok', `Created ${plural(made.length, 'invoice', 'invoices')} in AveeCare (${money(total)}). They are not in QuickBooks yet.`)];
  if (failed.length) nodes.push(callout('bad', failed));
  const warnings = invoiceWarnings(v);
  if (v.blocked > 0) nodes.push(el('p', 'review-sub', `${v.ready} of ${v.total} invoices are ready; ${plural(v.blocked, 'needs', 'need')} attention`));
  if (warnings.length) {
    const box = callout('warn', warnings);
    const actions = el('div', 'callout-actions');
    actions.append(fixLink('Open Billing in AveeCare', '/billing'));
    box.append(actions);
    nodes.push(box);
  }
  if (v.blocked > 0) {
    const label = el('label', 'check-row');
    const box = el('input');
    box.type = 'checkbox';
    box.dataset.analyticsId = 'qbc-inv-include-warned';
    box.addEventListener('change', () => { inv.includeAll = box.checked; });
    label.append(box, el('span', null, `Also send the ${plural(v.blocked, 'invoice', 'invoices')} with warnings`));
    nodes.push(label);
  }
  const ids = () => (inv.includeAll ? (v.allInvoiceIds || []) : (v.readyInvoiceIds || []));
  const row = el('div', 'send-actions');
  const send = el('button', 'btn primary', 'Send to QuickBooks');
  send.dataset.analyticsId = 'qbc-inv-send';
  send.disabled = !(v.allInvoiceIds || []).length;
  send.addEventListener('click', () => sendInvoices(ids(), warnings, send));
  row.append(send, fixLink('Undo this run in AveeCare', '/billing'));
  nodes.push(row);
  area.replaceChildren(...nodes);
}

async function sendInvoices(ids, warnings, btn) {
  if (inv.busy) return;
  const area = $('inv-review');
  if (!ids.length) { area.prepend(callout('bad', 'None of these invoices is ready. Tick "Also send" to send them anyway.')); return; }
  inv.busy = true;
  btn.disabled = true;
  try {
    const res = await aveecare('billing', 'qbd-queue-invoices', { invoiceIds: ids, ...(warnings.length ? { acknowledgedWarnings: warnings } : {}) });
    const n = res?.queued || 0;
    inv.created = null;
    if (n === 0) {
      area.replaceChildren(callout('warn', (res?.skipped || []).map((s) => s.reason).filter(Boolean).length ? res.skipped.map((s) => s.reason) : 'Nothing was queued.'));
      return;
    }
    await syncAfterQueue(area, `${plural(n, 'invoice', 'invoices')} queued for QuickBooks.`);
  } catch (e) {
    area.replaceChildren(callout('bad', errText(e)));
  } finally {
    inv.busy = false;
    btn.disabled = false;
  }
}

// ---------- recent sends: what is in QuickBooks, and fixing it ----------
//
// The same send history and undo the web app's "QuickBooks Desktop send
// history" panel uses (components/qbwc/qbd-send-history.tsx): billing's
// qbd-send-history, or payroll's for someone with Billing turned off, and
// qbd-queue-changes / queueQuickBooksDesktopChanges to remove records.
// Nothing changes QuickBooks here: each press queues work for the next sync.

const hist = { data: null, viaPayroll: false, busy: false, invoiceAction: 'delete' };

const recordStateWords = {
  'in-quickbooks': 'In QuickBooks',
  waiting: 'Waiting to send',
  refused: 'Refused by QuickBooks',
  'not-in-quickbooks': 'Not in QuickBooks',
};

const changeStateWords = {
  pending: 'Waiting for the next QuickBooks sync',
  running: 'Being done in QuickBooks now',
  retry: 'Will try again on the next sync',
  done: 'Done in QuickBooks',
  'already-gone': 'Already gone from QuickBooks',
  held: 'Needs your decision',
  conflict: 'Changed in QuickBooks since AveeCare sent it',
  failed: 'QuickBooks refused it',
  'kept-theirs': 'Kept the QuickBooks version',
  canceled: 'Canceled',
};

const isPayrollRecord = (t) => t === 'visit' || t === 'block';
/** Payroll records go through payroll-api, so someone with Billing off can still fix a payroll send. */
const writeApi = (payrollRecord) => (hist.viaPayroll || payrollRecord ? 'payroll' : 'billing');

async function loadHistory() {
  const area = $('hist-area');
  if (!area.childElementCount) area.replaceChildren(el('p', 'muted small', 'Reading AveeCare…'));
  try {
    try {
      hist.data = await aveecare('billing', 'qbd-send-history', { scope: 'all', days: 14 });
      hist.viaPayroll = false;
    } catch (e) {
      // Billing turned off for this person: payroll's own history still works.
      if (e?.code === 'SignedOut' || e?.code === 'Network') throw e;
      hist.data = await aveecare('payroll', 'getQuickBooksDesktopSendHistory', { days: 14 });
      hist.viaPayroll = true;
    }
    renderHistoryArea();
  } catch (e) {
    area.replaceChildren(callout('bad', errText(e)));
  }
}

function groupDecisions(changes) {
  const groups = new Map();
  for (const c of changes) {
    const key = c.state === 'conflict' ? `one:${c.id}` : `${c.state}:${c.reason || ''}:${c.kind}:${c.action}`;
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(c);
  }
  return [...groups.values()];
}

function decisionChoices(first) {
  if (first.state === 'conflict') return [['apply-ours', "Use AveeCare's version"], ['keep-theirs', 'Keep the QuickBooks version']];
  if (first.state === 'held' && ['paycheck', 'paycheck-unknown', 'billed'].includes(first.reason)) return [['go-ahead', 'Go ahead anyway'], ['cancel', 'Cancel']];
  return [['retry', 'Try again'], ['cancel', 'Cancel']];
}

async function resolveGroup(changes, decision, btn) {
  if (hist.busy) return;
  hist.busy = true;
  btn.disabled = true;
  let failed = 0;
  let firstError = '';
  for (const c of changes) {
    const api = writeApi(isPayrollRecord(c.recordType));
    try {
      await aveecare(api, api === 'payroll' ? 'resolveQuickBooksDesktopChange' : 'qbd-resolve-change', { changeId: c.id, decision });
    } catch (e) {
      failed += 1;
      if (!firstError) firstError = errText(e);
    }
  }
  hist.busy = false;
  await loadHistory();
  if (failed) $('hist-area').prepend(callout('bad', `${plural(failed, 'decision', 'decisions')} did not go through: ${firstError}`));
}

function decisionCard(changes) {
  const first = changes[0];
  const box = el('div', 'callout warn decision');
  const verb = first.kind === 'update' ? 'Update' : first.action === 'void' ? 'Void' : first.action === 'resend' ? 'Delete and send again' : 'Remove';
  box.append(el('p', 'review-sub', `${verb}: ${changeStateWords[first.state] || first.state}`));
  const names = changes.slice(0, 4).map((c) => c.label).join(', ') + (changes.length > 4 ? `, and ${changes.length - 4} more` : '');
  box.append(el('p', 'small', names));
  if (first.message) box.append(el('p', 'small', first.message));
  if (first.fix) box.append(el('p', 'small muted', first.fix));
  const actions = el('div', 'callout-actions');
  for (const [decision, label] of decisionChoices(first)) {
    const quiet = decision === 'cancel' || decision === 'keep-theirs';
    const b = el('button', quiet ? 'btn small-btn' : 'btn primary small-btn', changes.length > 1 && !quiet ? `${label} (${changes.length})` : label);
    b.type = 'button';
    b.dataset.analyticsId = `qbc-hist-${decision}`;
    b.addEventListener('click', () => resolveGroup(changes, decision, b));
    actions.append(b);
  }
  box.append(actions);
  return box;
}

const batchTitle = { payroll: 'Payroll', invoices: 'Invoices', payments: 'Payments' };

function batchSummary(b) {
  const count = {};
  for (const r of b.records) count[r.state] = (count[r.state] || 0) + 1;
  const parts = [];
  if (count['in-quickbooks']) parts.push(`${count['in-quickbooks']} in QuickBooks`);
  if (count.refused) parts.push(`${count.refused} refused`);
  if (count.waiting) parts.push(`${count.waiting} waiting`);
  if (count['not-in-quickbooks']) parts.push(`${count['not-in-quickbooks']} not in QuickBooks`);
  return parts.join(' · ');
}

async function removeRecords(batch, records, btn) {
  if (hist.busy || !records.length) return;
  hist.busy = true;
  btn.disabled = true;
  const api = writeApi(batch.kind === 'payroll');
  const op = api === 'payroll' ? 'queueQuickBooksDesktopChanges' : 'qbd-queue-changes';
  const request = {
    kind: 'undo',
    action: batch.kind === 'invoices' ? hist.invoiceAction : 'delete',
    records: records.map((r) => ({ recordType: r.recordType, recordId: r.recordId })),
  };
  let queued = 0;
  const refused = [];
  try {
    // The server queues what it has time for and names the rest; send the
    // rest again under the same job until none remain (as the web app does).
    const first = await aveecare(api, op, request);
    queued += (first?.queued || []).length;
    refused.push(...(first?.refused || []));
    let rest = first?.remaining || [];
    while (rest.length) {
      const next = await aveecare(api, op, { ...request, records: rest, jobId: first.jobId });
      queued += (next?.queued || []).length;
      refused.push(...(next?.refused || []));
      const left = next?.remaining || [];
      if (left.length >= rest.length) {
        refused.push(...left.map(() => ({ message: 'This record could not be queued. Try again.' })));
        break;
      }
      rest = left;
    }
  } catch (e) {
    refused.push({ message: errText(e) });
  }
  hist.busy = false;
  await loadHistory();
  const area = $('hist-area');
  if (refused.length) area.prepend(callout('bad', `${plural(refused.length, 'record was', 'records were')} not queued: ${refused[0].message}`));
  if (queued) await syncAfterQueue(area, `${plural(queued, 'record', 'records')} will be removed from QuickBooks.`);
}

function batchNode(b) {
  const d = el('details', 'batch');
  const sum = el('summary');
  sum.append(
    el('span', 'batch-title', `${batchTitle[b.kind] || b.kind} · ${when(b.queuedAt)}`),
    el('span', 'muted small', ` ${plural(b.records.length, 'record', 'records')} · ${batchSummary(b)}`),
  );
  d.append(sum);
  const list = el('ul', 'record-list');
  const remove = el('button', 'btn small-btn', 'Remove selected from QuickBooks');
  const boxes = [];
  for (const r of b.records) {
    const li = el('li', `record ${r.state}`);
    const removable = r.state === 'in-quickbooks' && !r.openChangeId;
    const box = el('input');
    box.type = 'checkbox';
    box.disabled = !removable;
    box.setAttribute('aria-label', r.label);
    box.addEventListener('change', () => { remove.disabled = !chosen().length; });
    boxes.push([box, r]);
    const main = el('div', 'record-main');
    const day = r.date ? shortDate(String(r.date).slice(0, 10)) : '';
    main.append(el('div', null, day && !r.label.includes(day) ? `${r.label} · ${day}` : r.label));
    const state = r.openChangeId ? 'A change is waiting for the next sync' : (recordStateWords[r.state] || r.state);
    main.append(el('div', 'muted small', r.detail ? `${state}: ${r.detail}` : state));
    li.append(box, main);
    list.append(li);
  }
  d.append(list);
  // What is ticked right now, read from the boxes at the moment of the press.
  const chosen = () => boxes.filter(([box]) => box.checked && !box.disabled).map(([, r]) => r);
  const confirmArea = el('div', 'review');
  const actions = el('div', 'send-actions');
  remove.type = 'button';
  remove.disabled = true;
  remove.dataset.analyticsId = 'qbc-hist-remove';
  remove.addEventListener('click', () => confirmRemoval(b, chosen(), confirmArea, actions));
  const inQb = boxes.filter(([box]) => !box.disabled);
  const all = el('button', 'link', `Select all ${inQb.length} in QuickBooks`);
  all.type = 'button';
  all.hidden = inQb.length < 2;
  all.dataset.analyticsId = 'qbc-hist-select-all';
  all.addEventListener('click', () => {
    for (const [box] of inQb) box.checked = true;
    remove.disabled = !chosen().length;
  });
  actions.append(remove, all);
  if (b.kind === 'invoices' && inQb.length) {
    const pick = el('select');
    pick.setAttribute('aria-label', 'How to remove invoices');
    for (const [v, label] of [['delete', 'Delete the invoices'], ['void', 'Void the invoices (kept at zero)']]) {
      const o = el('option', null, label);
      o.value = v;
      pick.append(o);
    }
    pick.value = hist.invoiceAction;
    pick.addEventListener('change', () => { hist.invoiceAction = pick.value; });
    actions.append(pick);
  }
  if (b.records.some((r) => r.state === 'refused')) {
    actions.append(fixLink('Fix in AveeCare', b.kind === 'payroll' ? '/payroll' : '/billing'));
    const again = el('button', 'btn small-btn', 'Sync again');
    again.type = 'button';
    again.title = 'Refused records stay queued and are tried again on every sync.';
    again.dataset.analyticsId = 'qbc-hist-sync-again';
    again.addEventListener('click', () => { showTab('sync'); startSync(); });
    actions.append(again);
  }
  d.append(actions, confirmArea);
  return d;
}

/** Name exactly what will be removed, and how, before anything is queued. */
function confirmRemoval(batch, records, area, actions) {
  if (!records.length) return;
  const how = batch.kind === 'invoices'
    ? (hist.invoiceAction === 'void' ? 'voided (kept in QuickBooks at zero)' : 'deleted from QuickBooks')
    : 'removed from QuickBooks';
  const box = el('div', 'callout warn decision');
  box.dataset.testid = 'qbc-remove-confirm';
  box.append(el('p', 'review-sub', `${plural(records.length, 'record', 'records')} will be ${how} on the next sync:`));
  const ul = el('ul', 'plain-list');
  for (const r of records.slice(0, 12)) ul.append(el('li', 'small', r.label));
  if (records.length > 12) ul.append(el('li', 'small', `and ${records.length - 12} more`));
  box.append(ul);
  box.append(el('p', 'small muted', 'Records changed in QuickBooks since, or already on a paycheck or paid, are not touched; each one tells you why.'));
  const row = el('div', 'callout-actions');
  const go = el('button', 'btn primary small-btn', batch.kind === 'invoices' && hist.invoiceAction === 'void' ? 'Void them' : 'Remove them');
  go.type = 'button';
  go.dataset.analyticsId = 'qbc-hist-remove-confirm';
  const keep = el('button', 'btn small-btn', 'Keep them');
  keep.type = 'button';
  keep.addEventListener('click', () => { area.replaceChildren(); actions.hidden = false; });
  go.addEventListener('click', () => removeRecords(batch, records, go));
  row.append(go, keep);
  box.append(row);
  actions.hidden = true;
  area.replaceChildren(box);
}

function renderHistoryArea() {
  const area = $('hist-area');
  const data = hist.data || { batches: [], changes: [] };
  const nodes = [];
  const needs = (data.changes || []).filter((c) => c.state === 'held' || c.state === 'conflict' || c.state === 'failed');
  for (const g of groupDecisions(needs)) nodes.push(decisionCard(g));
  const waiting = (data.changes || []).filter((c) => c.state === 'pending' || c.state === 'running' || c.state === 'retry');
  if (waiting.length) {
    const c = callout('ok', `${plural(waiting.length, 'change is', 'changes are')} waiting for the next QuickBooks sync.`);
    const actions = el('div', 'callout-actions');
    const go = el('button', 'btn small-btn', 'Sync now');
    go.type = 'button';
    go.dataset.analyticsId = 'qbc-hist-sync-now';
    go.addEventListener('click', () => { showTab('sync'); startSync(); });
    actions.append(go);
    c.append(actions);
    nodes.push(c);
  }
  for (const b of data.batches || []) if ((b.records || []).length) nodes.push(batchNode(b));
  if (!nodes.length) nodes.push(el('p', 'muted small', 'Nothing was sent to QuickBooks in the last 14 days.'));
  area.replaceChildren(...nodes);
}

// ---------- wiring ----------

function initSendTab() {
  const p = rememberedRange('qbc.payRange');
  $('pay-start').value = p.start;
  $('pay-end').value = p.end;
  const i = rememberedRange('qbc.invRange');
  $('inv-start').value = i.start;
  $('inv-end').value = i.end;
  $('pay-check').addEventListener('click', reviewPayroll);
  $('inv-check').addEventListener('click', reviewInvoices);
  $('hist-refresh').addEventListener('click', loadHistory);
  // Read on demand: when the Send tab is opened, and after each sync.
  document.querySelector('[data-tab=send]').addEventListener('click', loadHistory);
  listen('sync-finished', () => { if (!$('tab-send').hidden || hist.data) loadHistory(); });
  // A changed range makes the review on screen stale.
  for (const id of ['pay-start', 'pay-end']) $(id).addEventListener('change', () => { pay.check = null; $('pay-review').replaceChildren(); });
  for (const id of ['inv-start', 'inv-end']) $(id).addEventListener('change', () => { if (!inv.created) $('inv-review').replaceChildren(); });
}

document.addEventListener('DOMContentLoaded', initSendTab);
