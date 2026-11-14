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
  // A changed range makes the review on screen stale.
  for (const id of ['pay-start', 'pay-end']) $(id).addEventListener('change', () => { pay.check = null; $('pay-review').replaceChildren(); });
  for (const id of ['inv-start', 'inv-end']) $(id).addEventListener('change', () => { if (!inv.created) $('inv-review').replaceChildren(); });
}

document.addEventListener('DOMContentLoaded', initSendTab);
