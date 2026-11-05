// Plain JS, no build step. All text from other machines goes through esc().
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open, ask, message } = window.__TAURI__.dialog;

const S = {
  view: 'devices', // devices | device | browse | drive | settings
  me: { id: '', name: '' },
  driveRoot: '',
  devices: [],
  peer: null,        // device id open on the device / browse view
  remote: null,      // remote shares: null = loading, array, or { error }
  local: [],         // our shares, with `enabled` for this peer
  drive: [],         // LAN Drive items with status
  share: null,       // { id, name } being browsed
  path: '',          // path inside the share
  entries: null,     // null = loading, array, or { error }
  selected: new Set(),
  jobs: new Map(),   // job id -> { label, dest, done, total, error, finished }
  speed: new Map(),  // key -> { t, done, rate } for MB/s
  modal: null,       // { kind: 'pair-out'|'pair-in', id, name, code, waiting, error }
  nameDraft: null,   // settings input while editing
  nameNote: '',
};

const $view = document.getElementById('view');
const $jobs = document.getElementById('jobs');
const $modal = document.getElementById('modal');

const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
const nf = new Intl.NumberFormat('vi-VN', { maximumFractionDigits: 1 });
const size = (b) => {
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  while (b >= 1000 && i < u.length - 1) { b /= 1000; i++; }
  return `${nf.format(b)} ${u[i]}`;
};
const date = (s) => (s ? new Date(s * 1000).toLocaleDateString('vi-VN') : '');
const ago = (s) => {
  const m = Math.floor((Date.now() / 1000 - s) / 60);
  return m < 1 ? 'vừa xong' : m < 60 ? `${m} phút trước` : new Date(s * 1000).toLocaleString('vi-VN');
};
const device = (id) => S.devices.find((d) => d.id === id) || { id, name: 'Máy', online: false, paired: false };
const msg = (e) => String(e).replace(/^Error: /, '');
const fail = (e) => message(msg(e), { title: 'Không thực hiện được', kind: 'error' });

/** Smoothed transfer rate in bytes/s for `key`, fed with the running byte count. */
function track(key, done) {
  const t = performance.now();
  const s = S.speed.get(key);
  if (!s || done < s.done) { S.speed.set(key, { t, done, rate: 0 }); return 0; }
  const dt = (t - s.t) / 1000;
  if (dt >= 0.3) {
    const now = (done - s.done) / dt;
    s.rate = s.rate ? s.rate * 0.6 + now * 0.4 : now;
    Object.assign(s, { t, done });
  }
  return s.rate;
}
const rate = (key) => {
  const r = S.speed.get(key)?.rate;
  return r ? `${size(r)}/s` : '';
};

// ---------- data ----------

async function refresh() {
  const snap = await invoke('snapshot');
  S.me = { id: snap.id, name: snap.name };
  S.driveRoot = snap.drive_root;
  S.onDemand = snap.on_demand;
  S.driveError = snap.drive_error;
  S.devices = snap.devices.map((d) => ({ ...d, name: d.name || 'Máy chưa đặt tên' }));
  render();
}

async function loadDrive() {
  S.drive = await invoke('drive_list');
  for (const i of S.drive) if (i.status.state === 'syncing') track(`d:${i.peer}:${i.share}`, i.status.done);
}

async function loadDevice(quiet) {
  const id = S.peer;
  const [local] = await Promise.all([invoke('local_shares', { peer: id }), loadDrive()]);
  S.local = local;
  if (device(id).paired) {
    try {
      S.remote = await invoke('remote_shares', { peer: id });
    } catch (e) {
      if (!quiet || !Array.isArray(S.remote)) S.remote = { error: device(id).online ? msg(e) : 'Máy đang offline.' };
    }
  }
  if (S.view === 'device' && S.peer === id) render();
}

function openDevice(id) {
  Object.assign(S, { view: 'device', peer: id, remote: null, local: [] });
  render();
  loadDevice(false);
}

async function openFolder(share, path) {
  Object.assign(S, { view: 'browse', share, path, entries: null });
  S.selected.clear();
  render();
  try {
    S.entries = await invoke('remote_list', { peer: S.peer, share: share.id, path });
  } catch (e) {
    S.entries = { error: msg(e) };
  }
  render();
}

async function show(view) {
  S.view = view;
  if (view === 'drive') await loadDrive();
  if (view === 'settings') S.nameDraft = null;
  await refresh();
}

// ---------- views ----------

function render() {
  document.getElementById('me').textContent = S.me.name ? `Máy này: ${S.me.name}` : '';
  const section = S.view === 'drive' ? 'drive' : S.view === 'settings' ? 'settings' : 'devices';
  for (const b of document.querySelectorAll('.side [data-action]')) {
    if (b.dataset.action === section) b.setAttribute('aria-current', 'page');
    else b.removeAttribute('aria-current');
  }
  // Re-rendering replaces the list; keep its scroll position.
  const top = $view.querySelector('.scroll')?.scrollTop ?? 0;
  const focusId = document.activeElement?.id;
  $view.innerHTML = {
    devices: viewDevices, device: viewDevice, browse: viewBrowse, drive: viewDrive, settings: viewSettings,
  }[S.view]();
  const scroller = $view.querySelector('.scroll');
  if (scroller) scroller.scrollTop = top;
  if (focusId) document.getElementById(focusId)?.focus();
  renderJobs();
  renderModal();
}

function status(d) {
  return `<span class="status ${d.online ? 'on' : ''}">${d.online ? 'online' : 'offline'}</span>`;
}

function viewDevices() {
  const paired = S.devices.filter((d) => d.paired);
  const fresh = S.devices.filter((d) => !d.paired && d.online);
  const online = S.devices.filter((d) => d.online).length;
  return `
    <div class="head"><div>
      <h1>Thiết bị trong mạng</h1>
      <p class="sub">Tự tìm qua mạng LAN · ${online} máy đang online</p>
    </div></div>
    <div class="scroll">
    <section class="stack" aria-labelledby="h-paired">
      <h2 id="h-paired" class="eyebrow">Đã ghép</h2>
      ${paired.length ? `<div class="grid">${paired.map((d) => `
        <button type="button" class="card" data-action="device" data-id="${esc(d.id)}">
          <span class="name">${esc(d.name)}</span>${status(d)}
        </button>`).join('')}</div>`
      : '<p class="empty">Chưa ghép máy nào. Chọn một máy ở mục bên dưới để ghép.</p>'}
    </section>
    <section class="stack" aria-labelledby="h-new">
      <h2 id="h-new" class="eyebrow">Mới tìm thấy</h2>
      ${fresh.length ? fresh.map((d) => `
        <div class="card new">
          <div><div class="name">${esc(d.name)}</div><span class="status on">chưa ghép</span></div>
          <button type="button" class="btn primary" data-action="pair" data-id="${esc(d.id)}">Ghép máy</button>
        </div>`).join('')
      : '<p class="empty">Đang tìm… Máy khác cần mở app này và cùng mạng LAN.</p>'}
    </section>
    </div>`;
}

function viewDevice() {
  const d = device(S.peer);
  const crumbs = `<nav class="crumbs" aria-label="Vị trí"><button type="button" class="link" data-action="devices">Thiết bị</button> › ${esc(d.name)}</nav>`;
  const head = `<div class="head"><div><h1>${esc(d.name)}</h1><p class="sub">${status(d)}${d.paired ? ' · đã ghép' : ''}</p></div>
    ${d.paired ? '<button type="button" class="btn small" data-action="unpair">Bỏ ghép</button>' : ''}</div>`;
  if (!d.paired) {
    return `${crumbs}${head}<div class="scroll"><div class="panel"><h2>Chưa ghép với máy này</h2>
      <p class="sub">Ghép xong, hai máy mới thấy được folder chia sẻ cho nhau.</p>
      <button type="button" class="btn primary start" data-action="pair" data-id="${esc(d.id)}">Ghép máy</button></div></div>`;
  }
  let incoming;
  if (S.remote === null) incoming = '<p class="sub">Đang tải…</p>';
  else if (S.remote.error) incoming = `<p class="sub error">${esc(S.remote.error)}</p>`;
  else if (!S.remote.length) incoming = '<p class="sub">Máy này chưa chia sẻ folder nào cho bạn.</p>';
  else incoming = S.remote.map((s) => {
    const inDrive = S.drive.some((i) => i.peer === S.peer && i.share === s.id);
    return `<div class="row"><div class="grow"><span class="title">${esc(s.name)}</span></div>
      <button type="button" class="btn small" data-action="browse" data-id="${esc(s.id)}" data-name="${esc(s.name)}">Duyệt</button>
      ${inDrive ? '<span class="badge">Đã có trong LAN Drive</span>'
        : `<button type="button" class="btn small primary" data-action="add-drive" data-id="${esc(s.id)}" data-name="${esc(s.name)}">Thêm vào LAN Drive</button>`}</div>`;
  }).join('');
  const outgoing = S.local.map((s) => `
    <div class="row">
      <label class="grow" for="sh-${esc(s.id)}"><span class="title">${esc(s.name)}</span><span class="meta">${esc(s.path)}</span></label>
      <input id="sh-${esc(s.id)}" type="checkbox" role="switch" data-action="toggle" data-id="${esc(s.id)}" ${s.enabled ? 'checked' : ''}>
    </div>`).join('');
  return `${crumbs}${head}
    <div class="scroll"><div class="cols">
      <section class="panel" aria-labelledby="h-in"><h2 id="h-in">${esc(d.name)} chia sẻ cho bạn</h2>
        <p class="sub">Duyệt và tải về, hoặc thêm vào LAN Drive để tự cập nhật.</p>${incoming}</section>
      <section class="panel" aria-labelledby="h-out"><h2 id="h-out">Bạn chia sẻ cho ${esc(d.name)}</h2>
        <p class="sub">Chỉ máy đã ghép mới thấy. Tắt là máy kia không thấy nữa.</p>${outgoing}
        <button type="button" class="btn dashed" data-action="add-share">+ Chọn folder để chia sẻ</button></section>
    </div></div>`;
}

function viewBrowse() {
  const d = device(S.peer);
  const parts = S.path ? S.path.split('/') : [];
  const crumbs = `<nav class="crumbs" aria-label="Vị trí">
    <button type="button" class="link" data-action="devices">Thiết bị</button> ›
    <button type="button" class="link" data-action="device" data-id="${esc(d.id)}">${esc(d.name)}</button> ›
    ${parts.length ? `<button type="button" class="link" data-action="up" data-path="">${esc(S.share.name)}</button>` : esc(S.share.name)}
    ${parts.map((p, i) => ` › ${i < parts.length - 1
      ? `<button type="button" class="link" data-action="up" data-path="${esc(parts.slice(0, i + 1).join('/'))}">${esc(p)}</button>`
      : esc(p)}`).join('')}</nav>`;
  const n = S.selected.size;
  const count = Array.isArray(S.entries) ? `<p class="sub">${S.entries.length} mục</p>` : '';
  const head = `<div class="head"><div><h1>${esc(parts.at(-1) || S.share.name)}</h1>${count}</div>
    <button type="button" class="btn primary" data-action="download" ${n ? '' : 'disabled'}>${n ? `Tải ${n} mục đã chọn` : 'Chọn mục để tải'}</button></div>`;
  let body;
  if (S.entries === null) body = '<p class="sub">Đang tải…</p>';
  else if (S.entries.error) body = `<p class="sub error">${esc(S.entries.error)}</p>`;
  else if (!S.entries.length) body = '<p class="empty">Folder trống.</p>';
  else body = `<div class="scroll table-box"><table>
    <thead><tr><th class="check"><span class="sr">Chọn</span></th><th>Tên</th><th class="num">Kích thước</th><th>Sửa lần cuối</th></tr></thead>
    <tbody>${S.entries.map((e) => {
      const p = S.path ? `${S.path}/${e.name}` : e.name;
      const sel = S.selected.has(p);
      return `<tr class="${sel ? 'sel' : ''}">
        <td class="check"><input type="checkbox" aria-label="Chọn ${esc(e.name)}" data-action="select" data-path="${esc(p)}" ${sel ? 'checked' : ''}></td>
        <td>${e.is_dir ? `<button type="button" class="folder" data-action="enter" data-path="${esc(p)}">${esc(e.name)}</button>` : esc(e.name)}</td>
        <td class="num">${e.is_dir ? 'folder' : size(e.size)}</td>
        <td class="muted">${date(e.modified)}</td></tr>`;
    }).join('')}</tbody></table></div>`;
  return crumbs + head + body;
}

function driveState(i) {
  const s = i.status;
  if (s.state === 'syncing') {
    const pct = s.total ? Math.floor((s.done / s.total) * 100) : 0;
    const r = rate(`d:${i.peer}:${i.share}`);
    return `<div class="cell-progress"><span class="accent">${s.total ? `Đang đồng bộ · ${pct}%${r ? ` · ${r}` : ''}` : 'Đang kiểm tra thay đổi…'}</span>
      <div class="bar thin" role="progressbar" aria-label="Tiến độ ${esc(i.name)}" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${pct}"><div style="width:${pct}%"></div></div></div>`;
  }
  if (s.state === 'ondemand') return '<span class="ok">Sẵn sàng · file chỉ tải khi mở</span>';
  if (S.onDemand) return '<span class="warn">Máy đang offline · file đã mở trước đó vẫn dùng được</span>';
  if (s.state === 'synced') return `<span class="ok">Đã cập nhật · ${ago(s.last_sync)}</span>`;
  if (s.state === 'error') return `<span class="error">Lỗi: ${esc(s.message)}</span>`;
  return `<span class="warn">Chờ máy online${s.last_sync ? ` · bản cũ vẫn mở được` : ''}</span>`;
}

function viewDrive() {
  const rows = S.drive.map((i) => `<tr>
    <td class="strong">${esc(i.name)}</td>
    <td class="muted">${esc(i.peer_name || 'Máy')}</td>
    <td>${driveState(i)}</td>
    <td class="actions-cell">
      <button type="button" class="btn small" data-action="open-path" data-path="${esc(i.path)}">Mở</button>
      <button type="button" class="btn small" data-action="remove-drive" data-peer="${esc(i.peer)}" data-id="${esc(i.share)}">Gỡ</button></td></tr>`).join('');
  const problem = S.driveError ? `<p class="sub error" role="alert">LAN Drive on-demand chưa chạy được: ${esc(S.driveError)}</p>` : '';
  return `<div class="head"><div><h1>LAN Drive</h1>
      <p class="sub">${esc(S.driveRoot)} · mỗi máy một thư mục${S.onDemand ? ', file tải khi mở' : ', tự cập nhật khi máy kia online'}</p></div>
      <button type="button" class="btn primary" data-action="open-path" data-path="${esc(S.driveRoot)}">Mở folder LAN Drive</button></div>
    ${problem}
    ${S.drive.length ? `<div class="scroll table-box"><table>
      <thead><tr><th>Folder</th><th>Từ máy</th><th>Trạng thái</th><th><span class="sr">Thao tác</span></th></tr></thead>
      <tbody>${rows}</tbody></table></div>
      <p class="sub">${S.onDemand
        ? 'Mọi file hiện sẵn trong LAN Drive nhưng chỉ tải về khi bạn mở. Máy kia cần online lúc mở file lần đầu.'
        : 'LAN Drive là bản sao một chiều: sửa file trong đây sẽ bị ghi đè khi máy gốc thay đổi. File bị xoá ở máy gốc vẫn được giữ lại ở đây.'}</p>`
    : '<div class="scroll"><p class="empty">Chưa có folder nào. Vào một máy đã ghép và bấm "Thêm vào LAN Drive" ở folder bạn muốn giữ bản sao.</p></div>'}`;
}

function viewSettings() {
  const name = S.nameDraft ?? S.me.name;
  return `<div class="head"><h1>Cài đặt</h1></div>
    <div class="scroll stack">
      <form class="panel" data-form="rename">
        <h2>Tên máy này</h2>
        <p class="sub">Máy khác trong mạng sẽ thấy tên này.</p>
        <div class="inline">
          <label for="name" class="sr">Tên máy</label>
          <input id="name" class="input" maxlength="63" required value="${esc(name)}" data-input="name">
          <button type="submit" class="btn primary">Lưu</button>
        </div>
        ${S.nameNote ? `<p class="sub" role="status">${esc(S.nameNote)}</p>` : ''}
      </form>
      <div class="panel">
        <h2>LAN Drive</h2>
        <p class="sub">${esc(S.driveRoot)}</p>
        <button type="button" class="btn small start" data-action="open-path" data-path="${esc(S.driveRoot)}">Mở folder</button>
      </div>
      <div class="panel">
        <h2>Mã máy</h2>
        <p class="sub mono">${esc(S.me.id)}</p>
      </div>
    </div>`;
}

function renderJobs() {
  $jobs.innerHTML = [...S.jobs.entries()].map(([id, j]) => {
    const pct = j.total ? Math.floor((j.done / j.total) * 100) : 0;
    const r = rate(`j:${id}`);
    const state = j.error ? `<span class="error">Lỗi: ${esc(j.error)}</span>`
      : j.finished ? `Đã xong · lưu ở ${esc(j.dest)}`
      : `${size(j.done)} / ${j.total ? size(j.total) : '…'}${r ? ` · <strong>${r}</strong>` : ''} · ${pct}%`;
    return `<div class="job"><div class="job-top"><strong>${esc(j.label)}</strong><span>${state}</span>
      ${j.finished ? `<button type="button" class="btn small" data-action="open-path" data-path="${esc(j.dest)}">Mở folder</button>` : ''}
      ${j.finished || j.error ? `<button type="button" class="btn small" data-action="dismiss" data-id="${id}">Ẩn</button>` : ''}</div>
      <div class="bar" role="progressbar" aria-label="Tiến độ" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${pct}"><div style="width:${pct}%"></div></div></div>`;
  }).join('');
}

function renderModal() {
  const m = S.modal;
  if (!m) { $modal.innerHTML = ''; return; }
  const out = m.kind === 'pair-out';
  $modal.innerHTML = `<div class="backdrop"><div class="dialog" role="dialog" aria-modal="true" aria-labelledby="dlg-t">
    <h2 id="dlg-t">${out ? `Ghép với ${esc(m.name)}` : `${esc(m.name)} muốn ghép với máy này`}</h2>
    <p class="sub">${out ? 'Kiểm tra mã này giống với mã trên máy kia, bấm Xác nhận, rồi xác nhận tiếp trên máy kia.'
      : 'Chỉ đồng ý khi mã này giống mã đang hiện trên máy kia.'}</p>
    <p class="code" aria-label="Mã ghép ${esc(m.code.split('').join(' '))}">${esc(m.code)}</p>
    ${m.error ? `<p class="sub error">${esc(m.error)}</p>` : ''}
    ${m.waiting ? '<p class="sub">Đang chờ máy kia xác nhận…</p>' : ''}
    <div class="actions">
      <button type="button" class="btn" data-action="modal-no">${out ? 'Huỷ' : 'Từ chối'}</button>
      ${m.waiting ? '' : `<button type="button" class="btn primary" data-action="modal-yes">Xác nhận</button>`}
    </div></div></div>`;
}

// ---------- actions ----------

const actions = {
  devices: () => show('devices'),
  drive: () => show('drive'),
  settings: () => show('settings'),
  device: (el) => openDevice(el.dataset.id),
  pair: async (el) => {
    const id = el.dataset.id;
    S.modal = { kind: 'pair-out', id, name: device(id).name, code: await invoke('pair_code', { peer: id }) };
    render();
  },
  'modal-yes': async () => {
    const m = S.modal;
    if (m.kind === 'pair-in') {
      await invoke('answer_pair', { peer: m.id, accept: true });
      S.modal = null;
      return refresh();
    }
    m.waiting = true; m.error = null; render();
    try {
      const ok = await invoke('request_pair', { peer: m.id });
      if (S.modal !== m) return; // user cancelled meanwhile
      if (ok) { S.modal = null; await refresh(); return openDevice(m.id); }
      Object.assign(m, { waiting: false, error: 'Máy kia đã từ chối hoặc không trả lời.' });
    } catch (e) {
      Object.assign(m, { waiting: false, error: msg(e) });
    }
    render();
  },
  'modal-no': async () => {
    if (S.modal.kind === 'pair-in') await invoke('answer_pair', { peer: S.modal.id, accept: false });
    S.modal = null; render();
  },
  unpair: async () => {
    if (!(await ask('Hai máy sẽ không thấy folder của nhau nữa.', { title: 'Bỏ ghép?', kind: 'warning' }))) return;
    await invoke('unpair', { peer: S.peer });
    show('devices');
  },
  'add-share': async () => {
    const path = await open({ directory: true, multiple: false, title: 'Chọn folder để chia sẻ' });
    if (!path) return;
    try { await invoke('add_share', { path, peer: S.peer }); } catch (e) { fail(e); }
    loadDevice(true);
  },
  toggle: async (el) => {
    await invoke('set_share_peer', { share: el.dataset.id, peer: S.peer, enabled: el.checked });
    S.local = await invoke('local_shares', { peer: S.peer });
  },
  'add-drive': async (el) => {
    try { await invoke('add_to_drive', { peer: S.peer, share: el.dataset.id, name: el.dataset.name }); } catch (e) { fail(e); }
    loadDevice(true);
  },
  'remove-drive': async (el) => {
    const text = S.onDemand
      ? 'Folder sẽ biến mất khỏi LAN Drive trên máy này. File gốc trên máy kia không bị ảnh hưởng. File bạn tự lưu vào folder đó cũng bị xoá.'
      : 'Folder sẽ ngừng tự cập nhật. File đã có trong LAN Drive vẫn được giữ lại.';
    if (!(await ask(text, { title: 'Gỡ khỏi LAN Drive?', kind: 'warning' }))) return;
    await invoke('remove_from_drive', { peer: el.dataset.peer, share: el.dataset.id });
    await loadDrive(); render();
  },
  'open-path': (el) => invoke('open_path', { path: el.dataset.path }).catch(fail),
  browse: (el) => openFolder({ id: el.dataset.id, name: el.dataset.name }, ''),
  enter: (el) => openFolder(S.share, el.dataset.path),
  up: (el) => openFolder(S.share, el.dataset.path),
  select: (el) => {
    el.checked ? S.selected.add(el.dataset.path) : S.selected.delete(el.dataset.path);
    render();
  },
  download: async () => {
    const paths = [...S.selected];
    try {
      const [job, dest] = await invoke('download', { peer: S.peer, share: S.share.id, paths });
      S.jobs.set(job, { label: `Tải ${paths.length} mục từ ${device(S.peer).name}`, dest, done: 0, total: 0 });
      S.selected.clear(); render();
    } catch (e) { fail(e); }
  },
  dismiss: (el) => { S.jobs.delete(Number(el.dataset.id)); renderJobs(); },
};

document.addEventListener('click', (ev) => {
  const el = ev.target.closest('[data-action]');
  if (!el || el.disabled || el.type === 'checkbox') return;
  actions[el.dataset.action]?.(el);
});
document.addEventListener('change', (ev) => {
  const el = ev.target;
  if (el.type === 'checkbox' && el.dataset.action) actions[el.dataset.action]?.(el);
});
document.addEventListener('input', (ev) => {
  if (ev.target.dataset.input === 'name') S.nameDraft = ev.target.value;
});
document.addEventListener('submit', async (ev) => {
  if (ev.target.dataset.form !== 'rename') return;
  ev.preventDefault();
  try {
    await invoke('set_name', { name: S.nameDraft ?? S.me.name });
    S.nameDraft = null;
    S.nameNote = 'Đã lưu. Máy khác sẽ thấy tên mới sau vài giây.';
  } catch (e) { S.nameNote = msg(e); }
  refresh();
});

// Lists change on the other device without telling us: re-check the open device every few seconds.
setInterval(() => {
  if (S.view === 'device' && device(S.peer).paired) loadDevice(true);
}, 4000);

let driveBusy = false;
listen('core', async ({ payload: ev }) => {
  if (ev.type === 'devices') {
    refresh();
  } else if (ev.type === 'pairRequest') {
    S.modal = { kind: 'pair-in', id: ev.id, name: ev.name, code: ev.code };
    renderModal();
  } else if (ev.type === 'progress') {
    const j = S.jobs.get(ev.job);
    if (j) { j.done = ev.done; j.total = ev.total; track(`j:${ev.job}`, ev.done); renderJobs(); }
  } else if (ev.type === 'jobDone') {
    const j = S.jobs.get(ev.job);
    if (j) { j.finished = !ev.error; j.error = ev.error; if (!ev.error) j.done = j.total; renderJobs(); }
  } else if (ev.type === 'drive' && (S.view === 'drive' || S.view === 'device') && !driveBusy) {
    driveBusy = true; // progress events come fast; one reload at a time is enough
    try { await loadDrive(); render(); } finally { driveBusy = false; }
  }
});

refresh();
