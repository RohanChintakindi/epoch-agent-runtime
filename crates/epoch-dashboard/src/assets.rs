pub const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="dark">
  <title>Epoch Runtime Inspector</title>
  <link rel="stylesheet" href="/assets/app.css">
</head>
<body>
  <header class="topbar">
    <div class="brand"><span class="mark">E</span><span>Epoch</span><span class="muted">Runtime Inspector</span></div>
    <div class="connection"><span id="connection-dot" class="dot"></span><span id="connection-text">Connecting</span><button id="refresh" type="button">Refresh</button></div>
  </header>
  <div class="shell">
    <aside class="sidebar" aria-label="Sessions">
      <div class="section-head"><h1>Sessions</h1><span id="session-count" class="count">0</span></div>
      <label class="field"><span>Status</span><select id="session-status"><option value="">All</option><option>running</option><option>suspended</option><option>completed</option><option>failed</option></select></label>
      <div id="sessions" class="session-list" aria-live="polite"></div>
    </aside>
    <main>
      <section id="welcome" class="welcome"><h2>Select a session</h2><p>Inspect trusted runtime state without exposing payloads, provider output, or bearer material.</p></section>
      <div id="inspector" hidden>
        <section class="session-header">
          <div><p class="eyebrow">Selected session</p><h2 id="session-id"></h2></div>
          <dl id="session-meta" class="inline-meta"></dl>
        </section>
        <div class="primary-grid">
          <section class="panel branch-panel">
            <div class="panel-head"><h3>Branch tree</h3><span id="branch-count" class="count"></span></div>
            <div id="branches" class="branch-tree"></div>
          </section>
          <section class="panel timeline-panel">
            <div class="panel-head"><div><h3>Event timeline</h3><p id="timeline-scope" class="subtle"></p></div><div class="pager"><button id="timeline-prev" type="button">Prev</button><span id="timeline-page"></span><button id="timeline-next" type="button">Next</button></div></div>
            <div class="filters">
              <label class="field"><span>Actor</span><select id="actor-filter"><option value="">All</option><option>agent</option><option>supervisor</option><option>tool</option><option>gateway</option><option>operator</option></select></label>
              <label class="field"><span>Kind</span><input id="kind-filter" maxlength="128" placeholder="e.g. safe_point"></label>
              <label class="field"><span>Status</span><select id="event-status"><option value="">All</option><option>started</option><option>succeeded</option><option>failed</option><option>denied</option><option>unknown</option></select></label>
            </div>
            <div class="table-wrap"><table><thead><tr><th>Seq</th><th>Time</th><th>Actor</th><th>Kind</th><th>Status</th><th>Epoch</th></tr></thead><tbody id="timeline"></tbody></table></div>
          </section>
        </div>
        <div class="detail-grid">
          <section class="panel"><div class="panel-head"><h3>Checkpoints & restores</h3><span id="epoch-count" class="count"></span></div><div id="epochs" class="stack"></div></section>
          <section class="panel"><div class="panel-head"><h3>Semantic diffs</h3><span id="diff-count" class="count"></span></div><div id="diffs" class="stack"></div></section>
          <section class="panel"><div class="panel-head"><h3>Capabilities</h3><span class="privacy">handles excluded</span></div><div id="capabilities" class="stack"></div></section>
          <section class="panel"><div class="panel-head"><h3>Effect history</h3><span class="privacy">provider content excluded</span></div><div id="effects" class="stack"></div></section>
          <section class="panel"><div class="panel-head"><h3>Backend support</h3><span id="host" class="subtle"></span></div><div id="backends" class="stack"></div></section>
          <section class="panel"><div class="panel-head"><h3>Benchmark evidence</h3><span id="benchmark-status" class="subtle"></span></div><div id="benchmarks" class="stack"></div></section>
        </div>
      </div>
    </main>
  </div>
  <script src="/assets/app.js" defer></script>
</body>
</html>
"#;

pub const APP_CSS: &str = r#":root {
  color-scheme: dark;
  --bg: #0d0f12;
  --surface: #12151a;
  --surface-2: #171b21;
  --border: #2a3039;
  --border-soft: #20252c;
  --text: #e3e7ec;
  --muted: #8f98a5;
  --faint: #626b77;
  --accent: #9fb7d4;
  --ok: #74b98a;
  --warn: #d2ab6a;
  --bad: #d47a7a;
  font-family: Inter, ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  font-size: 14px;
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--text); min-width: 320px; }
button, input, select { font: inherit; color: inherit; }
button, input, select { border: 1px solid var(--border); background: var(--surface-2); border-radius: 3px; }
button { padding: 6px 10px; cursor: pointer; }
button:hover, button:focus-visible { border-color: var(--accent); outline: none; }
button:disabled { color: var(--faint); cursor: default; border-color: var(--border-soft); }
input, select { min-height: 30px; padding: 4px 8px; width: 100%; }
h1, h2, h3, p, dl, dd { margin: 0; }
h1, h2, h3 { font-weight: 600; letter-spacing: -0.01em; }
h1, h3 { font-size: 13px; text-transform: uppercase; letter-spacing: .045em; }
h2 { font-size: 18px; }
.topbar { height: 48px; border-bottom: 1px solid var(--border); display: flex; align-items: center; justify-content: space-between; padding: 0 16px; background: var(--surface); position: sticky; top: 0; z-index: 5; }
.brand, .connection { display: flex; align-items: center; gap: 8px; }
.mark { width: 24px; height: 24px; display: grid; place-items: center; border: 1px solid var(--border); font-weight: 700; }
.muted, .subtle { color: var(--muted); }
.connection { color: var(--muted); font-size: 12px; }
.dot { width: 7px; height: 7px; border-radius: 50%; background: var(--warn); }
.dot.ok { background: var(--ok); }
.dot.bad { background: var(--bad); }
.shell { min-height: calc(100vh - 48px); display: grid; grid-template-columns: 260px minmax(0, 1fr); }
.sidebar { border-right: 1px solid var(--border); background: var(--surface); padding: 12px; }
.section-head, .panel-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; min-height: 32px; }
.count, .privacy { color: var(--muted); border: 1px solid var(--border-soft); border-radius: 10px; padding: 1px 7px; font-size: 11px; }
.privacy { border-radius: 3px; }
.field { display: grid; gap: 4px; color: var(--muted); font-size: 11px; }
.sidebar > .field { margin: 8px 0 12px; }
.session-list { display: grid; gap: 4px; }
.session { width: 100%; text-align: left; padding: 8px; display: grid; gap: 5px; background: transparent; }
.session.selected { border-color: var(--accent); background: var(--surface-2); }
.session-id, .mono { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
.session-id { font-size: 11px; overflow: hidden; text-overflow: ellipsis; }
.session-foot { display: flex; justify-content: space-between; color: var(--muted); font-size: 11px; }
main { min-width: 0; padding: 16px; }
.welcome { min-height: 50vh; display: grid; place-content: center; gap: 8px; text-align: center; color: var(--muted); }
.welcome h2 { color: var(--text); }
.session-header { border-bottom: 1px solid var(--border); padding: 0 0 12px; margin-bottom: 12px; display: flex; justify-content: space-between; align-items: end; gap: 16px; }
.eyebrow { color: var(--muted); text-transform: uppercase; font-size: 10px; letter-spacing: .08em; margin-bottom: 4px; }
.session-header h2 { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; overflow-wrap: anywhere; }
.inline-meta { display: flex; gap: 16px; }
.inline-meta div { display: grid; gap: 2px; }
.inline-meta dt { color: var(--muted); font-size: 10px; text-transform: uppercase; }
.inline-meta dd { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; }
.primary-grid { display: grid; grid-template-columns: minmax(210px, 1fr) minmax(560px, 3fr); gap: 12px; align-items: stretch; }
.detail-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; margin-top: 12px; }
.panel { border: 1px solid var(--border); background: var(--surface); padding: 12px; min-width: 0; }
.primary-grid .panel { min-height: 410px; }
.branch-tree { display: grid; gap: 4px; margin-top: 8px; }
.branch-node { border-left: 1px solid var(--border); padding-left: 8px; }
.branch-button { width: 100%; text-align: left; background: transparent; padding: 7px; display: grid; gap: 3px; }
.branch-button.selected { background: var(--surface-2); border-color: var(--accent); }
.branch-line { display: flex; justify-content: space-between; gap: 6px; }
.branch-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.status { font-size: 10px; text-transform: uppercase; letter-spacing: .04em; color: var(--muted); }
.status.succeeded, .status.completed, .status.committed, .status.active, .status.supported { color: var(--ok); }
.status.failed, .status.denied, .status.revoked, .status.unsupported { color: var(--bad); }
.status.started, .status.running, .status.unknown { color: var(--warn); }
.filters { display: grid; grid-template-columns: 120px minmax(140px, 1fr) 120px; gap: 8px; margin: 8px 0; }
.pager { display: flex; align-items: center; gap: 6px; color: var(--muted); font-size: 11px; }
.pager button { padding: 4px 7px; }
.table-wrap { overflow: auto; border-top: 1px solid var(--border-soft); }
table { width: 100%; border-collapse: collapse; font-size: 12px; }
th, td { text-align: left; padding: 7px 8px; border-bottom: 1px solid var(--border-soft); white-space: nowrap; }
th { color: var(--muted); font-size: 10px; text-transform: uppercase; letter-spacing: .05em; font-weight: 500; }
td.kind { white-space: normal; min-width: 170px; }
.stack { display: grid; gap: 6px; margin-top: 8px; }
.record { border-top: 1px solid var(--border-soft); padding-top: 7px; display: grid; gap: 5px; }
.record:first-child { border-top: 0; padding-top: 0; }
.record-title { display: flex; align-items: baseline; justify-content: space-between; gap: 8px; }
.record-title strong { font-size: 12px; overflow-wrap: anywhere; }
.kv { display: flex; flex-wrap: wrap; gap: 4px 12px; color: var(--muted); font-size: 11px; }
.kv span { overflow-wrap: anywhere; }
.component { display: grid; grid-template-columns: minmax(100px, 1fr) auto auto; gap: 8px; padding: 4px 6px; background: var(--surface-2); font-size: 11px; }
.change { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 11px; overflow-wrap: anywhere; border-left: 2px solid var(--border); padding-left: 6px; }
.empty, .error { border: 1px dashed var(--border); color: var(--muted); padding: 12px; font-size: 12px; }
.error { color: var(--bad); border-color: #563737; }
@media (max-width: 980px) {
  .shell { grid-template-columns: 210px minmax(0, 1fr); }
  .primary-grid, .detail-grid { grid-template-columns: 1fr; }
  .primary-grid .panel { min-height: 0; }
}
@media (max-width: 680px) {
  .topbar { padding: 0 8px; }
  .brand .muted { display: none; }
  .shell { display: block; }
  .sidebar { border-right: 0; border-bottom: 1px solid var(--border); }
  .session-list { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  main { padding: 8px; }
  .session-header { align-items: start; flex-direction: column; }
  .inline-meta { flex-wrap: wrap; }
  .filters { grid-template-columns: 1fr; }
}
"#;

pub const APP_JS: &str = r"'use strict';

const POLL_MS = 5000;
const TIMELINE_LIMIT = 50;
const state = { sessionId: null, branchId: null, timelineOffset: 0, loading: false };

const byId = (id) => document.getElementById(id);
const make = (tag, className, text) => {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text !== undefined && text !== null) element.textContent = String(text);
  return element;
};
const clear = (element) => element.replaceChildren();
const shortId = (value) => value ? value.slice(0, 8) : '—';
const time = (value) => value === null || value === undefined ? '—' : new Date(value).toLocaleTimeString();
const status = (value) => make('span', `status ${value}`, value);
const empty = (target, message) => { clear(target); target.append(make('div', 'empty', message)); };
const error = (target, message) => { clear(target); target.append(make('div', 'error', message)); };

async function api(path) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 4000);
  try {
    const response = await fetch(path, { cache: 'no-store', credentials: 'same-origin', signal: controller.signal });
    const body = await response.json();
    if (!response.ok) throw new Error(body.message || body.error || `HTTP ${response.status}`);
    setConnection(true);
    return body;
  } finally {
    clearTimeout(timeout);
  }
}

function setConnection(connected, message) {
  byId('connection-dot').className = `dot ${connected ? 'ok' : 'bad'}`;
  byId('connection-text').textContent = connected ? 'Local / read only' : (message || 'Unavailable');
}

function record(title, stateValue) {
  const root = make('article', 'record');
  const heading = make('div', 'record-title');
  heading.append(make('strong', 'mono', title));
  if (stateValue) heading.append(status(stateValue));
  root.append(heading);
  return root;
}

function kv(items) {
  const row = make('div', 'kv');
  items.forEach(([label, value]) => row.append(make('span', '', `${label}: ${value ?? '—'}`)));
  return row;
}

async function loadSessions(quiet = false) {
  const query = new URLSearchParams({ limit: '100', offset: '0' });
  const filter = byId('session-status').value;
  if (filter) query.set('status', filter);
  try {
    const data = await api(`/api/v1/sessions?${query}`);
    renderSessions(data.items);
    byId('session-count').textContent = data.page.has_more ? `${data.items.length}+` : data.items.length;
    if (!quiet && !state.sessionId && data.items.length === 1) selectSession(data.items[0].session_id);
  } catch (cause) {
    setConnection(false, 'State unavailable');
    error(byId('sessions'), cause.message);
  }
}

function renderSessions(items) {
  const target = byId('sessions');
  clear(target);
  if (!items.length) return empty(target, 'No sessions match this filter.');
  items.forEach((session) => {
    const button = make('button', `session${state.sessionId === session.session_id ? ' selected' : ''}`);
    button.type = 'button';
    button.append(make('span', 'session-id', session.session_id));
    const foot = make('span', 'session-foot');
    foot.append(status(session.state), make('span', '', `${session.branch_count} branches · ${session.epoch_count} epochs`));
    button.append(foot);
    button.addEventListener('click', () => selectSession(session.session_id));
    target.append(button);
  });
}

async function selectSession(sessionId) {
  state.sessionId = sessionId;
  state.branchId = null;
  state.timelineOffset = 0;
  byId('welcome').hidden = true;
  byId('inspector').hidden = false;
  await loadSessions(true);
  await refreshSession();
}

async function refreshSession() {
  if (!state.sessionId || state.loading) return;
  state.loading = true;
  try {
    const base = `/api/v1/sessions/${encodeURIComponent(state.sessionId)}`;
    const [detail, epochs, diffs, capabilities, effects] = await Promise.all([
      api(base), api(`${base}/epochs?limit=100&offset=0`), api(`${base}/diffs?limit=50&offset=0`),
      api(`${base}/capabilities`), api(`${base}/effects`)
    ]);
    renderSession(detail);
    renderEpochs(epochs);
    renderDiffs(diffs);
    renderCapabilities(capabilities);
    renderEffects(effects);
    if (!state.branchId || !detail.branches.some((branch) => branch.branch_id === state.branchId)) {
      state.branchId = detail.branches[0]?.branch_id || null;
      state.timelineOffset = 0;
    }
    renderBranches(detail.branches, detail.branches_truncated);
    await loadTimeline();
  } catch (cause) {
    setConnection(false, 'State unavailable');
    error(byId('branches'), cause.message);
  } finally {
    state.loading = false;
  }
}

function renderSession(detail) {
  byId('session-id').textContent = detail.session_id;
  const meta = byId('session-meta');
  clear(meta);
  [['state', detail.state], ['policy', detail.policy_revision], ['revision', detail.revision], ['updated', time(detail.updated_at_unix_ms)]].forEach(([label, value]) => {
    const group = make('div'); group.append(make('dt', '', label), make('dd', '', value)); meta.append(group);
  });
  byId('branch-count').textContent = detail.branches_truncated ? `${detail.branches.length}+` : detail.branches.length;
}

function renderBranches(branches, truncated) {
  const target = byId('branches');
  clear(target);
  if (!branches.length) return empty(target, 'No branches recorded.');
  const children = new Map();
  branches.forEach((branch) => {
    const key = branch.parent_branch_id || 'root';
    if (!children.has(key)) children.set(key, []);
    children.get(key).push(branch);
  });
  const visited = new Set();
  const append = (branch, parent, depth) => {
    if (visited.has(branch.branch_id) || depth > 32) return;
    visited.add(branch.branch_id);
    const wrapper = make('div', 'branch-node');
    const button = make('button', `branch-button${state.branchId === branch.branch_id ? ' selected' : ''}`);
    button.type = 'button';
    const line = make('span', 'branch-line');
    line.append(make('span', 'branch-name mono', branch.name || shortId(branch.branch_id)), status(branch.state));
    button.append(line, make('span', 'subtle mono', branch.fork_epoch_id ? `fork @ ${shortId(branch.fork_epoch_id)} · seq ${branch.fork_point_sequence}` : 'root branch'));
    button.addEventListener('click', () => { state.branchId = branch.branch_id; state.timelineOffset = 0; renderBranches(branches, truncated); loadTimeline(); });
    wrapper.append(button);
    (children.get(branch.branch_id) || []).forEach((child) => append(child, wrapper, depth + 1));
    parent.append(wrapper);
  };
  (children.get('root') || []).forEach((branch) => append(branch, target, 0));
  branches.filter((branch) => !visited.has(branch.branch_id)).forEach((branch) => append(branch, target, 0));
  if (truncated) target.append(make('div', 'empty', 'Branch tree truncated at the server safety limit.'));
}

async function loadTimeline() {
  const body = byId('timeline');
  if (!state.branchId) { clear(body); return; }
  const query = new URLSearchParams({ limit: String(TIMELINE_LIMIT), offset: String(state.timelineOffset) });
  const actor = byId('actor-filter').value;
  const kind = byId('kind-filter').value.trim();
  const eventStatus = byId('event-status').value;
  if (actor) query.set('actor', actor);
  if (kind) query.set('kind', kind);
  if (eventStatus) query.set('status', eventStatus);
  try {
    const data = await api(`/api/v1/branches/${encodeURIComponent(state.branchId)}/timeline?${query}`);
    renderTimeline(data);
  } catch (cause) {
    clear(body);
    const row = make('tr'); const cell = make('td', 'error', cause.message); cell.colSpan = 6; row.append(cell); body.append(row);
  }
}

function renderTimeline(data) {
  const body = byId('timeline');
  clear(body);
  byId('timeline-scope').textContent = `${shortId(data.branch_id)} · payloads redacted`;
  byId('timeline-page').textContent = `${data.page.offset + 1}–${data.page.offset + data.items.length}`;
  byId('timeline-prev').disabled = data.page.offset === 0;
  byId('timeline-next').disabled = !data.page.has_more;
  if (!data.items.length) {
    const row = make('tr'); const cell = make('td', 'subtle', 'No events match these filters.'); cell.colSpan = 6; row.append(cell); body.append(row); return;
  }
  data.items.forEach((event) => {
    const row = make('tr');
    row.append(make('td', 'mono', event.sequence), make('td', '', time(event.occurred_at_unix_ms)), make('td', '', event.actor), make('td', 'kind mono', event.kind));
    const statusCell = make('td'); statusCell.append(status(event.status)); row.append(statusCell, make('td', 'mono', shortId(event.epoch_id)));
    body.append(row);
  });
}

function renderEpochs(data) {
  const target = byId('epochs'); clear(target); byId('epoch-count').textContent = data.page.has_more ? `${data.items.length}+` : data.items.length;
  if (!data.items.length) return empty(target, 'No checkpoints committed for this session.');
  data.items.forEach((epoch) => {
    const root = record(shortId(epoch.epoch_id), epoch.status);
    root.append(kv([['branch', shortId(epoch.branch_id)], ['seq', epoch.sequence], ['backend', epoch.backend], ['effects', epoch.effect_frontier], ['capabilities', epoch.capability_frontier]]));
    epoch.components.forEach((component) => { const line = make('div', 'component'); line.append(make('span', 'mono', component.kind), status(component.status), make('span', 'subtle', `${component.byte_length} B`)); root.append(line); });
    if (!epoch.components.length) root.append(make('div', 'empty', 'No snapshot components.'));
    epoch.restore_outcomes.forEach((restore) => root.append(kv([['restore', restore.status], ['branch', shortId(restore.branch_id)], ['at', time(restore.occurred_at_unix_ms)]])));
    target.append(root);
  });
}

function renderDiffs(data) {
  const target = byId('diffs'); clear(target); byId('diff-count').textContent = data.page.has_more ? `${data.items.length}+` : data.items.length;
  if (!data.items.length) return empty(target, 'No persisted semantic diffs available.');
  data.items.forEach((diff) => {
    const root = record(`${shortId(diff.left_epoch_id)} → ${shortId(diff.right_epoch_id)}`, diff.identical === true ? 'identical' : 'changed');
    root.append(kv([['changes', diff.change_count], ['schema', diff.schema_version], ['values', 'redacted']]));
    diff.changes.forEach((change) => root.append(make('div', 'change', `${change.classification} · ${change.section} · ${change.path}`)));
    if (diff.unsupported_sections.length) root.append(make('p', 'subtle', `Unsupported: ${diff.unsupported_sections.join(', ')}`));
    target.append(root);
  });
}

function renderCapabilities(data) {
  const target = byId('capabilities'); clear(target);
  if (!data.current.length && !data.audit.length) return empty(target, 'No capability state or decisions recorded.');
  data.current.forEach((capability) => {
    const root = record(`${capability.action} · ${capability.resource}`, capability.state);
    root.append(kv([['branch', shortId(capability.branch_id)], ['subject', capability.subject], ['uses', capability.remaining_uses], ['budget', capability.remaining_budget_units], ['policy', capability.policy_revision]])); target.append(root);
  });
  if (data.audit.length) target.append(make('p', 'eyebrow', 'Decision audit'));
  data.audit.forEach((decision) => { const root = record(`#${decision.sequence} · ${decision.action}`, decision.outcome); root.append(kv([['reason', decision.reason], ['branch', shortId(decision.branch_id)], ['budget', decision.budget_units], ['at', time(decision.decided_at_unix_ms)]])); target.append(root); });
}

function renderEffects(data) {
  const target = byId('effects'); clear(target);
  if (!data.intents.length) return empty(target, 'No external effect intents recorded.');
  data.intents.forEach((effect) => {
    const root = record(`${effect.action} · ${effect.resource}`, effect.state);
    root.append(kv([['branch', shortId(effect.branch_id)], ['operation', effect.operation_id], ['replay', effect.replay_key], ['attempts', effect.attempts.length]]));
    effect.transitions.forEach((entry) => root.append(make('div', 'component', `transition ${entry.sequence} · ${entry.state} · ${time(entry.occurred_at_unix_ms)}`)));
    effect.attempts.forEach((attempt) => root.append(kv([['attempt', attempt.attempt_no], ['state', attempt.state], ['started', time(attempt.started_at_unix_ms)], ['completed', time(attempt.completed_at_unix_ms)]])));
    target.append(root);
  });
}

async function loadBackends() {
  const target = byId('backends');
  try {
    const data = await api('/api/v1/backends'); clear(target); byId('host').textContent = `${data.host_os}/${data.architecture}`;
    data.backends.forEach((backend) => { const root = record(backend.id, backend.status); root.append(kv([['scope', backend.scope], ['registered', backend.registered], ['dependency', backend.dependency_detected]]), make('p', 'subtle', backend.reason)); target.append(root); });
  } catch (cause) { error(target, cause.message); }
}

async function loadBenchmarks() {
  const target = byId('benchmarks');
  try {
    const data = await api('/api/v1/benchmarks'); clear(target);
    if (!data.available) { byId('benchmark-status').textContent = 'unavailable'; return empty(target, `Benchmark results unavailable: ${data.reason}.`); }
    byId('benchmark-status').textContent = `${data.reports.length} reports`;
    if (!data.reports.length) return empty(target, 'Results directory is present but has no valid bounded reports.');
    data.reports.forEach((report) => { const root = record(`${report.suite} · ${report.backend}`, report.failed ? 'failed' : 'completed'); root.append(kv([['trace', report.trace_mode], ['samples', report.repetitions], ['ok', report.succeeded], ['unsupported', report.unsupported], ['failed', report.failed], ['p50', report.p50_ns === null ? '—' : `${report.p50_ns} ns`], ['p95', report.p95_ns === null ? '—' : `${report.p95_ns} ns`]])); target.append(root); });
  } catch (cause) { error(target, cause.message); }
}

byId('refresh').addEventListener('click', () => { loadSessions(true); refreshSession(); loadBackends(); loadBenchmarks(); });
byId('session-status').addEventListener('change', () => loadSessions());
byId('actor-filter').addEventListener('change', () => { state.timelineOffset = 0; loadTimeline(); });
byId('event-status').addEventListener('change', () => { state.timelineOffset = 0; loadTimeline(); });
byId('kind-filter').addEventListener('change', () => { state.timelineOffset = 0; loadTimeline(); });
byId('timeline-prev').addEventListener('click', () => { state.timelineOffset = Math.max(0, state.timelineOffset - TIMELINE_LIMIT); loadTimeline(); });
byId('timeline-next').addEventListener('click', () => { state.timelineOffset += TIMELINE_LIMIT; loadTimeline(); });

loadSessions();
loadBackends();
loadBenchmarks();
setInterval(() => { if (!document.hidden) { loadSessions(true); refreshSession(); } }, POLL_MS);
";
