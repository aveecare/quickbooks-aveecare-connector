// Settings → Drive time and mileage: how travel between clients reaches
// QuickBooks. These are the very settings on AveeCare's Settings → QuickBooks
// Desktop card, read and saved through the same operations
// (getQuickBooksDesktopStatus / saveQuickBooksDesktopPayrollItemMap on payroll,
// qbd-status / qbd-save-invoice-settings on billing). Nothing here is stored in
// the Connector.
//
// QuickBooks Desktop does not let other software put mileage on a paycheck, so
// mileage is paid as a separate check per caregiver (when turned on), drive time
// as hours on an hourly payroll item, and billed mileage as invoice lines.
'use strict';

const travel = { payroll: null, billing: null, saving: false };

/** QuickBooks names arrive XML-escaped from discovery; show and save them plain. */
const plainQbName = (raw) => String(raw)
  .replace(/&#x([0-9a-fA-F]+);/g, (_m, hex) => String.fromCodePoint(parseInt(hex, 16)))
  .replace(/&#(\d+);/g, (_m, dec) => String.fromCodePoint(parseInt(dec, 10)))
  .replace(/&quot;/g, '"').replace(/&apos;/g, "'")
  .replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&amp;/g, '&');

const byName = (a, b) => a.localeCompare(b, 'en-US', { sensitivity: 'base' });

/**
 * The stored item slots exactly as the web card resends them: every string key
 * except the legacy mileage slot. Overtime settings and mileage checks are
 * objects or separate keys the server keeps when a save leaves them out.
 */
function storedSlots(payrollItemMap) {
  const slots = {};
  for (const [k, v] of Object.entries(payrollItemMap || {})) {
    if (k === 'mileage' || k === 'overtimeMode' || k === 'overtimeModeSince') continue;
    if (typeof v === 'string') slots[k] = v;
  }
  return slots;
}

function storedMileageChecks(payrollItemMap) {
  const raw = payrollItemMap && typeof payrollItemMap.mileageChecks === 'object' && payrollItemMap.mileageChecks
    ? payrollItemMap.mileageChecks : {};
  const bankAccount = typeof raw.bankAccount === 'string' ? raw.bankAccount : '';
  const expenseAccount = typeof raw.expenseAccount === 'string' ? raw.expenseAccount : '';
  // Same rule the send uses (lib/qbwc/mileage-checks.ts): on only with both accounts.
  return { enabled: raw.enabled === true && !!bankAccount.trim() && !!expenseAccount.trim(), bankAccount, expenseAccount };
}

function picker(id, choices, value, emptyLabel, analyticsId) {
  const s = el('select');
  s.id = id;
  s.dataset.analyticsId = analyticsId;
  const none = el('option', null, emptyLabel);
  none.value = '';
  s.append(none);
  const names = [...new Set(choices)].sort(byName);
  // A saved name the last sync no longer read stays visible, never silently blank.
  if (value && !names.includes(value)) names.unshift(value);
  for (const n of names) {
    const o = el('option', null, n);
    o.value = n;
    s.append(o);
  }
  s.value = value || '';
  return s;
}

function savedNote(text) {
  const p = el('p', 'muted small', text);
  p.dataset.testid = 'qbc-travel-saved';
  return p;
}

async function loadTravel(note) {
  const area = $('travel-area');
  if (!area) return;
  area.replaceChildren(el('p', 'muted small', 'Reading your settings from AveeCare…'));
  try {
    const [payroll, billing] = await Promise.all([
      aveecare('payroll', 'getQuickBooksDesktopStatus', {}),
      aveecare('billing', 'qbd-status', {}),
    ]);
    travel.payroll = payroll || {};
    travel.billing = billing || {};
    renderTravel(note);
  } catch (e) {
    area.replaceChildren(callout('bad', errText(e)));
  }
}

function renderTravel(note) {
  const area = $('travel-area');
  const st = travel.payroll;
  if (!st.connected) {
    area.replaceChildren(callout('warn', 'QuickBooks Desktop is not set up in AveeCare yet. Set it up in AveeCare under Settings → QuickBooks Desktop, then come back.'));
    return;
  }
  const map = st.payrollItemMap || {};
  const discovered = st.discovered || {};
  const payrollItems = (discovered.payrollItems || []).map(String);
  const bankAccounts = (discovered.bankAccounts || []).map(plainQbName);
  const expenseAccounts = (discovered.expenseAccounts || []).map(plainQbName);
  const serviceItems = (travel.billing.serviceItems || []).map(String);
  const nodes = [];
  if (note) nodes.push(savedNote(note));
  if (!payrollItems.length && !serviceItems.length) {
    nodes.push(callout('warn', 'The Connector has not read your QuickBooks items yet. Run a sync once with QuickBooks open, then come back.'));
  }

  // 1. Drive time: hours on an hourly payroll item.
  {
    const box = el('div', 'setting column');
    box.dataset.testid = 'qbc-travel-drive';
    box.append(el('div', 'setting-title', 'Pay drive time under'));
    box.append(el('div', 'muted small', 'Paid drive time goes to QuickBooks as hours on this hourly payroll item, at the rate set on it in QuickBooks. Not picked: drive time is not sent.'));
    const sel = picker('travel-drive-item', payrollItems, map.driveTime || '', 'Not sent to QuickBooks', 'qbc-travel-drive-item');
    const save = el('button', 'btn small-btn', 'Save');
    save.type = 'button';
    save.dataset.analyticsId = 'qbc-travel-drive-save';
    save.disabled = true;
    sel.addEventListener('change', () => { save.disabled = sel.value === (map.driveTime || ''); });
    save.addEventListener('click', () => saveDriveItem(sel.value, save));
    const row = el('div', 'inline-form');
    row.append(sel, save);
    box.append(row);
    nodes.push(box);
  }

  // 2. Mileage pay: a separate check per caregiver.
  {
    const saved = storedMileageChecks(map);
    const box = el('div', 'setting column');
    box.dataset.testid = 'qbc-travel-checks';
    box.append(el('div', 'setting-title', 'Pay mileage with a check'));
    box.append(el('div', 'muted small', 'QuickBooks Desktop does not let other software add mileage to a paycheck. With this on, each send writes one check per caregiver for their paid mileage, separate from the paycheck, booked to the expense account you pick.'));
    const onLabel = el('label', 'check-line');
    const on = el('input');
    on.type = 'checkbox';
    on.id = 'travel-checks-on';
    on.checked = saved.enabled;
    on.dataset.analyticsId = 'qbc-travel-checks-on';
    onLabel.append(on, el('span', null, ' Write mileage checks in QuickBooks'));
    const bank = picker('travel-checks-bank', bankAccounts, saved.bankAccount, 'Pick a bank account', 'qbc-travel-checks-bank');
    const expense = picker('travel-checks-expense', expenseAccounts, saved.expenseAccount, 'Pick an expense account', 'qbc-travel-checks-expense');
    const accounts = el('div', 'stack');
    const bankLine = el('label', 'small', 'Pay from bank account');
    bankLine.append(bank);
    const expLine = el('label', 'small', 'Book mileage to expense account');
    expLine.append(expense);
    accounts.append(bankLine, expLine);
    if (!bankAccounts.length || !expenseAccounts.length) {
      accounts.append(el('p', 'muted small', 'No accounts read from QuickBooks yet. Run a sync, then come back.'));
    }
    const save = el('button', 'btn small-btn', 'Save mileage setting');
    save.type = 'button';
    save.dataset.analyticsId = 'qbc-travel-checks-save';
    const hint = el('p', 'form-error');
    hint.hidden = true;
    const confirmArea = el('div');
    const sync = () => {
      accounts.hidden = !on.checked;
      const changed = on.checked !== saved.enabled
        || (on.checked && (bank.value !== saved.bankAccount || expense.value !== saved.expenseAccount));
      const missing = on.checked && (!bank.value || !expense.value);
      save.disabled = !changed || missing;
      hint.hidden = !missing;
      hint.textContent = missing ? 'Pick both accounts to turn this on.' : '';
      confirmArea.replaceChildren();
    };
    for (const n of [on, bank, expense]) n.addEventListener('change', sync);
    save.addEventListener('click', () => {
      const next = { enabled: on.checked, bankAccount: bank.value.trim(), expenseAccount: expense.value.trim() };
      if (next.enabled && !saved.enabled) confirmChecks(next, confirmArea, save);
      else saveMileageChecks(next, save);
    });
    box.append(onLabel, accounts, hint, save, confirmArea);
    sync();
    nodes.push(box);
  }

  // 3. Billed mileage: the service item for errand and travel invoice lines.
  {
    const current = travel.billing.mileageServiceItemName || '';
    const box = el('div', 'setting column');
    box.dataset.testid = 'qbc-travel-invoice';
    box.append(el('div', 'setting-title', 'Bill mileage under'));
    box.append(el('div', 'muted small', 'Errand and travel lines on invoices book against this QuickBooks service item. Not picked: they book against your main service item.'));
    const sel = picker('travel-invoice-item', serviceItems, current, 'Main service item', 'qbc-travel-invoice-item');
    const save = el('button', 'btn small-btn', 'Save');
    save.type = 'button';
    save.dataset.analyticsId = 'qbc-travel-invoice-save';
    save.disabled = true;
    sel.addEventListener('change', () => { save.disabled = sel.value === current; });
    save.addEventListener('click', () => saveInvoiceMileageItem(sel.value, save));
    const row = el('div', 'inline-form');
    row.append(sel, save);
    box.append(row);
    nodes.push(box);
  }

  area.replaceChildren(...nodes);
}

/** Turning checks on can pay mileage twice if it is already paid another way: say so first. */
function confirmChecks(next, area, save) {
  const box = el('div', 'callout warn decision');
  box.dataset.testid = 'qbc-travel-checks-confirm';
  box.append(el('p', 'review-sub', 'Write mileage checks in QuickBooks?'));
  box.append(el('p', 'small', `From the next send, each caregiver whose visits carry paid mileage gets a separate check for that mileage, drawn on ${next.bankAccount}. It is not part of the paycheck. If you already pay mileage another way (a payroll addition, or by hand), stop doing that for these visits, or the mileage is paid twice.`));
  const row = el('div', 'callout-actions');
  const go = el('button', 'btn primary small-btn', 'Write mileage checks');
  go.type = 'button';
  go.dataset.analyticsId = 'qbc-travel-checks-confirm';
  const cancel = el('button', 'btn small-btn', 'Cancel');
  cancel.type = 'button';
  cancel.addEventListener('click', () => { area.replaceChildren(); save.hidden = false; });
  go.addEventListener('click', () => saveMileageChecks(next, go));
  row.append(go, cancel);
  box.append(row);
  save.hidden = true;
  area.replaceChildren(box);
}

/**
 * The payroll item save replaces every item slot with what is sent, so the
 * slots are read fresh right before it: a change made on the web page a minute
 * ago is resent as it is now, not as it was when this card opened.
 */
async function freshSlots() {
  const st = await aveecare('payroll', 'getQuickBooksDesktopStatus', {});
  return storedSlots((st || {}).payrollItemMap);
}

async function runSave(btn, work, note) {
  if (travel.saving) return;
  travel.saving = true;
  btn.disabled = true;
  const label = btn.textContent;
  btn.textContent = 'Saving…';
  try {
    await work();
    await loadTravel(note);
  } catch (e) {
    btn.textContent = label;
    btn.disabled = false;
    const err = callout('bad', `That was not saved. ${errText(e)}`);
    err.dataset.testid = 'qbc-travel-error';
    btn.closest('.setting')?.append(err);
  } finally {
    travel.saving = false;
  }
}

function saveDriveItem(name, btn) {
  return runSave(btn, async () => {
    const slots = await freshSlots();
    await aveecare('payroll', 'saveQuickBooksDesktopPayrollItemMap', { ...slots, driveTime: name });
  }, name ? `Saved. Drive time is paid under ${name} from the next send.` : 'Saved. Drive time is no longer sent to QuickBooks.');
}

function saveMileageChecks(next, btn) {
  return runSave(btn, async () => {
    const slots = await freshSlots();
    await aveecare('payroll', 'saveQuickBooksDesktopPayrollItemMap', { ...slots, mileageChecks: next });
  }, next.enabled ? 'Saved. Mileage goes to QuickBooks as a check from the next send.' : 'Saved. Mileage is no longer sent to QuickBooks.');
}

function saveInvoiceMileageItem(name, btn) {
  return runSave(btn, async () => {
    await aveecare('billing', 'qbd-save-invoice-settings', { mileageServiceItemName: name });
  }, name ? `Saved. Errands and travel book against ${name}.` : 'Saved. Errands and travel book against your main service item.');
}

function initTravel() {
  // Read on demand: each time Settings is opened, and after a sync (which may
  // have read new items or accounts out of QuickBooks).
  document.querySelector('[data-tab=settings]').addEventListener('click', () => loadTravel());
  listen('sync-finished', () => { if (!$('tab-settings').hidden) loadTravel(); });
}

document.addEventListener('DOMContentLoaded', initTravel);
