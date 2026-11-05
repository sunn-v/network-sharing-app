// Plain JS, no build step. All text from other machines goes through esc().
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open, ask, message } = window.__TAURI__.dialog;

const S = {
  view: 'devices', // devices | device | browse
  me: { id: '', name: '' },
  devices: [],
  peer: null,        // device id open on the device / browse view
  remote: null,      // remote shares: null = loading, array, or { error }
  local: [],         // our shares, with `enabled` for this peer
  share: null,       // { id, name } being browsed
  path: '',          // path inside the share
  entries: null,     // null = loading, array, or { error }
  selected: new Set(),
  jobs: new Map(),   // job id -> { label, dest, done, total, error, finished }
  modal: null,       // { kind: 'pair-out'|'pair-in', id, name, code, waiting, error }
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
const device = (id) => S.devices.find((d) => d.id === id) || { id, name: 'Máy', online: false, paired: false };
const msg = (e) => String(e).replace(/^Error: /, '');

// ---------- data ----------

async function refresh() {
  const snap = await invoke('snapshot');
  S.me = { id: snap.id, name: snap.name };
  S.devices = snap.devices.map((d) => ({ ...d, name: d.name || 'Máy chưa đặt tên' }));
  render();
}

async function openDevice(id) {
  Object.assign(S, { view: 'device', peer: id, remote: null, local: [] });
  render();
  S.local = await invoke('local_shares', { peer: id });
  render();
  if (!device(id).paired) return;
  try {
    S.remote = await invoke('remote_shares', { peer: id });
  } catch (e) {
    S.remote = { error: device(id).online ? msg(e) : 'Máy đang offline.' };
  }
  if (S.view === 'device' && S.peer === id) render();
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

// ---------- views ----------

function render() {
  document.getElementById('me').textContent = S.me.name ? `Máy này: ${S.me.name}` : '';
  $view.innerHTML = S.view === 'devices' ? viewDevices() : S.view === 'device' ? viewDevice() : viewBrowse();
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
      : '<p class="empty">Đang tìm… Máy khác cần mở app này và cùng mạng LAN. Trên macOS, kiểm tra quyền Mạng cục bộ (Local Network) trong Cài đặt hệ thống.</p>'}
    </section>`;
}

function viewDevice() {
  const d = device(S.peer);
  const crumbs = `<nav class="crumbs" aria-label="Vị trí"><button type="button" class="link" data-action="home">Thiết bị</button> › ${esc(d.name)}</nav>`;
  const head = `<div class="head"><div><h1>${esc(d.name)}</h1><p class="sub">${status(d)}${d.paired ? ' · đã ghép' : ''}</p></div>
    ${d.paired ? '<button type="button" class="btn small" data-action="unpair">Bỏ ghép</button>' : ''}</div>`;
  if (!d.paired) {
    return `${crumbs}${head}<div class="panel"><h2>Chưa ghép với máy này</h2>
      <p class="sub">Ghép xong, hai máy mới thấy được folder chia sẻ cho nhau.</p>
      <button type="button" class="btn primary" data-action="pair" data-id="${esc(d.id)}" style="align-self:flex-start">Ghép máy</button></div>`;
  }
  let incoming;
  if (S.remote === null) incoming = '<p class="sub">Đang tải…</p>';
  else if (S.remote.error) incoming = `<p class="sub error">${esc(S.remote.error)}</p>`;
  else if (!S.remote.length) incoming = '<p class="sub">Máy này chưa chia sẻ folder nào cho bạn.</p>';
  else incoming = S.remote.map((s) => `
    <div class="row"><div class="grow"><span class="title">${esc(s.name)}</span></div>
      <button type="button" class="btn small" data-action="browse" data-id="${esc(s.id)}" data-name="${esc(s.name)}">Duyệt</button></div>`).join('');
  const outgoing = S.local.map((s) => `
    <div class="row">
      <label class="grow" for="sh-${esc(s.id)}"><span class="title">${esc(s.name)}</span><span class="meta">${esc(s.path)}</span></label>
      <input id="sh-${esc(s.id)}" type="checkbox" role="switch" data-action="toggle" data-id="${esc(s.id)}" ${s.enabled ? 'checked' : ''}>
    </div>`).join('');
  return `${crumbs}${head}
    <div class="cols">
      <section class="panel" aria-labelledby="h-in"><h2 id="h-in">${esc(d.name)} chia sẻ cho bạn</h2>
        <p class="sub">Duyệt và tải về máy này.</p>${incoming}</section>
      <section class="panel" aria-labelledby="h-out"><h2 id="h-out">Bạn chia sẻ cho ${esc(d.name)}</h2>
        <p class="sub">Chỉ máy đã ghép mới thấy. Tắt là máy kia không thấy nữa.</p>${outgoing}
        <button type="button" class="btn dashed" data-action="add-share">+ Chọn folder để chia sẻ</button></section>
    </div>`;
}

function viewBrowse() {
  const d = device(S.peer);
  const parts = S.path ? S.path.split('/') : [];
  const crumbs = `<nav class="crumbs" aria-label="Vị trí">
    <button type="button" class="link" data-action="home">Thiết bị</button> ›
    <button type="button" class="link" data-action="device" data-id="${esc(d.id)}">${esc(d.name)}</button> ›
    ${parts.length ? `<button type="button" class="link" data-action="up" data-path="">${esc(S.share.name)}</button>` : esc(S.share.name)}
    ${parts.map((p, i) => ` › ${i < parts.length - 1
      ? `<button type="button" class="link" data-action="up" data-path="${esc(parts.slice(0, i + 1).join('/'))}">${esc(p)}</button>`
      : esc(p)}`).join('')}</nav>`;
  const n = S.selected.size;
  const head = `<div class="head"><h1>${esc(parts.at(-1) || S.share.name)}</h1>
    <button type="button" class="btn primary" data-action="download" ${n ? '' : 'disabled'}>${n ? `Tải ${n} mục đã chọn` : 'Chọn mục để tải'}</button></div>`;
  let body;
  if (S.entries === null) body = '<p class="sub">Đang tải…</p>';
  else if (S.entries.error) body = `<p class="sub error">${esc(S.entries.error)}</p>`;
  else if (!S.entries.length) body = '<p class="empty">Folder trống.</p>';
  else body = `<div class="table-box"><table>
    <thead><tr><th class="check"><span class="sr">Chọn</span></th><th>Tên</th><th class="num">Kích thước</th><th>Sửa lần cuối</th></tr></thead>
    <tbody>${S.entries.map((e) => {
      const p = S.path ? `${S.path}/${e.name}` : e.name;
      const sel = S.selected.has(p);
      return `<tr class="${sel ? 'sel' : ''}">
        <td class="check"><input type="checkbox" aria-label="Chọn ${esc(e.name)}" data-action="select" data-path="${esc(p)}" ${sel ? 'checked' : ''}></td>
        <td>${e.is_dir ? `<button type="button" class="folder" data-action="enter" data-path="${esc(p)}">${esc(e.name)}</button>` : esc(e.name)}</td>
        <td class="num">${e.is_dir ? 'folder' : size(e.size)}</td>
        <td class="num" style="text-align:left">${date(e.modified)}</td></tr>`;
    }).join('')}</tbody></table></div>`;
  return crumbs + head + body;
}

function renderJobs() {
  $jobs.innerHTML = [...S.jobs.entries()].map(([id, j]) => {
    const pct = j.total ? Math.floor((j.done / j.total) * 100) : 0;
    const state = j.error ? `<span class="error">Lỗi: ${esc(j.error)}</span>`
      : j.finished ? `Đã xong · lưu ở ${esc(j.dest)}`
      : `${size(j.done)} / ${j.total ? size(j.total) : '…'} · mất mạng giữa chừng thì tải lại sẽ chạy tiếp`;
    return `<div class="job"><div class="job-top"><strong>${esc(j.label)}</strong><span>${state}</span>
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
  home: () => { S.view = 'devices'; refresh(); },
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
    S.view = 'devices'; refresh();
  },
  'add-share': async () => {
    const path = await open({ directory: true, multiple: false, title: 'Chọn folder để chia sẻ' });
    if (!path) return;
    try { await invoke('add_share', { path, peer: S.peer }); } catch (e) { message(msg(e), { title: 'Không thực hiện được', kind: 'error' }); }
    S.local = await invoke('local_shares', { peer: S.peer }); render();
  },
  toggle: async (el) => {
    await invoke('set_share_peer', { share: el.dataset.id, peer: S.peer, enabled: el.checked });
    S.local = await invoke('local_shares', { peer: S.peer });
  },
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
    } catch (e) { message(msg(e), { title: 'Không thực hiện được', kind: 'error' }); }
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

listen('core', ({ payload: ev }) => {
  if (ev.type === 'devices') {
    refresh();
  } else if (ev.type === 'pairRequest') {
    S.modal = { kind: 'pair-in', id: ev.id, name: ev.name, code: ev.code };
    renderModal();
  } else if (ev.type === 'progress') {
    const j = S.jobs.get(ev.job);
    if (j) { j.done = ev.done; j.total = ev.total; renderJobs(); }
  } else if (ev.type === 'jobDone') {
    const j = S.jobs.get(ev.job);
    if (j) { j.finished = !ev.error; j.error = ev.error; if (!ev.error) j.done = j.total; renderJobs(); }
  }
});

refresh();
