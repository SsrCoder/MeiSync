/* MeiSync admin console – dependency-free SPA. */

const API = '/api/admin'
const TOKEN_KEY = 'meisync.admin.token'
const FILE_PATH_KEY = 'meisync.admin.filePath'
const app = document.getElementById('app')

const ROUTES = {
  overview: { title: 'Overview', sub: 'Server health, storage and recent activity', icon: 'grid' },
  users: { title: 'Users', sub: 'Accounts, roles, quotas and access', icon: 'users' },
  files: { title: 'Files', sub: 'Browse every namespace in the node', icon: 'folder' },
  sessions: { title: 'Sessions', sub: 'Active bearer tokens', icon: 'key' },
  devices: { title: 'Sync devices', sub: 'Obsidian vaults connected to this node', icon: 'monitor' },
  audit: { title: 'Audit log', sub: 'Security relevant events', icon: 'list' },
  settings: { title: 'Server', sub: 'Runtime configuration and endpoints', icon: 'settings' },
}

const state = {
  token: localStorage.getItem(TOKEN_KEY) || '',
  me: null,
  route: routeFromHash(),
  overview: null,
  version: '',
  users: [],
  userSearch: '',
  filePath: localStorage.getItem(FILE_PATH_KEY) || '/private',
  fileOwner: '',
  sessions: [],
  devices: [],
  audit: [],
}


class ApiError extends Error {
  constructor(status, message) {
    super(message)
    this.name = 'ApiError'
    this.status = status
  }
}

// ---------------------------------------------------------------------------
// utilities
// ---------------------------------------------------------------------------

function routeFromHash() {
  const value = location.hash.replace(/^#\/?/, '').split('?')[0]
  return ROUTES[value] ? value : 'overview'
}

function esc(value) {
  return String(value ?? '').replace(/[&<>"']/g, (char) => {
    switch (char) {
      case '&':
        return '&amp;'
      case '<':
        return '&lt;'
      case '>':
        return '&gt;'
      case '"':
        return '&quot;'
      default:
        return '&#39;'
    }
  })
}

function fmtBytes(bytes) {
  const value = Number(bytes) || 0
  if (value < 1024) return `${value} B`
  const units = ['KB', 'MB', 'GB', 'TB', 'PB']
  let unit = -1
  let scaled = value
  do {
    scaled /= 1024
    unit += 1
  } while (scaled >= 1024 && unit < units.length - 1)
  return `${scaled >= 100 ? scaled.toFixed(0) : scaled.toFixed(scaled >= 10 ? 1 : 2)} ${units[unit]}`
}

function fmtDate(value) {
  if (!value) return '—'
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return String(value)
  return date.toLocaleString(undefined, {
    year: 'numeric',
    month: 'short',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

function fmtRelative(value) {
  if (!value) return '—'
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '—'
  const seconds = Math.round((Date.now() - date.getTime()) / 1000)
  if (seconds < 60) return 'just now'
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`
  if (seconds < 86400 * 30) return `${Math.floor(seconds / 86400)}d ago`
  return fmtDate(value)
}

function initials(name) {
  const clean = String(name || '?').replace(/[^a-zA-Z0-9._-]/g, '')
  return (clean.slice(0, 2) || '?').toUpperCase()
}

function icon(name, size = 16) {
  const paths = {
    grid: '<rect x="3" y="3" width="7" height="7" rx="1.4"/><rect x="14" y="3" width="7" height="7" rx="1.4"/><rect x="3" y="14" width="7" height="7" rx="1.4"/><rect x="14" y="14" width="7" height="7" rx="1.4"/>',
    users: '<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/>',
    folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
    key: '<circle cx="7.5" cy="15.5" r="4.5"/><path d="m11 12 9-9"/><path d="m17 4 3 3"/><path d="m14 7 3 3"/>',
    monitor: '<rect x="2" y="3" width="20" height="14" rx="2"/><path d="M8 21h8"/><path d="M12 17v4"/>',
    list: '<path d="M8 6h13"/><path d="M8 12h13"/><path d="M8 18h13"/><path d="M3 6h.01"/><path d="M3 12h.01"/><path d="M3 18h.01"/>',
    settings: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .34 1.87l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.7 1.7 0 0 0-1.87-.34 1.7 1.7 0 0 0-1 1.55V21a2 2 0 1 1-4 0v-.09A1.7 1.7 0 0 0 9 19.36a1.7 1.7 0 0 0-1.87.34l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.7 1.7 0 0 0 4.64 15a1.7 1.7 0 0 0-1.55-1H3a2 2 0 1 1 0-4h.09A1.7 1.7 0 0 0 4.64 9a1.7 1.7 0 0 0-.34-1.87l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.7 1.7 0 0 0 9 4.64a1.7 1.7 0 0 0 1-1.55V3a2 2 0 1 1 4 0v.09a1.7 1.7 0 0 0 1 1.55 1.7 1.7 0 0 0 1.87-.34l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.7 1.7 0 0 0 19.36 9a1.7 1.7 0 0 0 1.55 1H21a2 2 0 1 1 0 4h-.09a1.7 1.7 0 0 0-1.51 1z"/>',
    refresh: '<path d="M20 11a8 8 0 0 0-14.9-3M4 5v4h4"/><path d="M4 13a8 8 0 0 0 14.9 3M20 19v-4h-4"/>',
    plus: '<path d="M12 5v14"/><path d="M5 12h14"/>',
    search: '<circle cx="11" cy="11" r="7"/><path d="m21 21-4.3-4.3"/>',
    logout: '<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><path d="m16 17 5-5-5-5"/><path d="M21 12H9"/>',
    chevron: '<path d="m9 18 6-6-6-6"/>',
    file: '<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><path d="M14 2v6h6"/><path d="M8 13h8"/><path d="M8 17h6"/>',
    trash: '<path d="M3 6h18"/><path d="M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2"/><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/><path d="M10 11v6"/><path d="M14 11v6"/>',
    edit: '<path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z"/>',
    eye: '<path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z"/><circle cx="12" cy="12" r="3"/>',
    x: '<path d="M18 6 6 18"/><path d="m6 6 12 12"/>',
    check: '<path d="m20 6-11 11-5-5"/>',
    shield: '<path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><path d="m9 12 2 2 4-4"/>',
    server: '<rect x="2" y="3" width="20" height="7" rx="2"/><rect x="2" y="14" width="20" height="7" rx="2"/><path d="M6 6.5h.01"/><path d="M6 17.5h.01"/>',
    database: '<ellipse cx="12" cy="5" rx="8" ry="3"/><path d="M4 5v14c0 1.7 3.6 3 8 3s8-1.3 8-3V5"/><path d="M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3"/>',
    activity: '<path d="M3 12h4l3 8 4-16 3 8h4"/>',
    download: '<path d="M12 3v12"/><path d="m7 10 5 5 5-5"/><path d="M5 21h14"/>',
    external: '<path d="M15 3h6v6"/><path d="M10 14 21 3"/><path d="M21 14v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5"/>',
    upload: '<path d="M12 21V9"/><path d="m7 14 5-5 5 5"/><path d="M5 3h14"/>',
  }
  return `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${paths[name] || paths.grid}</svg>`
}

// ---------------------------------------------------------------------------
// API
// ---------------------------------------------------------------------------

async function api(path, options = {}) {
  const { method = 'GET', body, raw = false, auth = true } = options
  const headers = {}
  if (auth && state.token) headers.Authorization = `Bearer ${state.token}`
  let payload = body
  if (body !== undefined && typeof body !== 'string' && !(body instanceof Blob) && !(body instanceof FormData)) {
    headers['content-type'] = 'application/json'
    payload = JSON.stringify(body)
  }
  const response = await fetch(`${API}${path}`, { method, headers, body: payload, cache: 'no-store' })
  if (response.status === 401 && auth) {
    logout(false)
    throw new ApiError(401, 'Your session expired. Please sign in again.')
  }
  if (!response.ok) {
    let message = `${response.status} ${response.statusText}`
    try {
      const parsed = await response.json()
      message = parsed.message || parsed.error || message
    } catch {
      /* non JSON error body */
    }
    throw new ApiError(response.status, message)
  }
  if (raw) return response
  if (response.status === 204) return null
  return response.json()
}

function logout(notify = true) {
  const token = state.token
  if (token) {
    // Best effort: revoke the bearer token server side as well. A failed call must not keep the
    // user trapped in the console when the token is already invalid.
    fetch(`${API}/logout`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${token}` },
      cache: 'no-store',
    }).catch(() => {})
  }
  state.token = ''
  state.me = null
  state.overview = null
  state.version = ''
  state.users = []
  state.sessions = []
  state.devices = []
  state.audit = []
  localStorage.removeItem(TOKEN_KEY)
  if (notify) toast('Signed out', 'success')
  renderLogin()
}

// ---------------------------------------------------------------------------
// toasts / modals
// ---------------------------------------------------------------------------

let toastHost = null

function toast(message, kind = 'info', timeout = 4200) {
  if (!toastHost) {
    toastHost = document.createElement('div')
    toastHost.className = 'toast-stack'
    document.body.appendChild(toastHost)
  }
  const node = document.createElement('div')
  node.className = `toast ${kind}`
  node.innerHTML = `<span class="mark"></span><span>${esc(message)}</span>`
  toastHost.appendChild(node)
  window.setTimeout(() => {
    node.style.opacity = '0'
    node.style.transform = 'translateY(6px)'
    node.style.transition = '0.2s ease'
    window.setTimeout(() => node.remove(), 220)
  }, timeout)
}

function openModal({ title, body, footer = '', wide = false, onClose = null }) {
  const overlay = document.createElement('div')
  overlay.className = 'modal-overlay'
  overlay.innerHTML = `
    <div class="modal ${wide ? 'wide' : ''}" role="dialog" aria-modal="true" aria-label="${esc(title)}">
      <div class="modal-head">
        <h2>${esc(title)}</h2>
        <button class="icon-btn close" type="button" data-close aria-label="Close">${icon('x')}</button>
      </div>
      <div class="modal-body">${body}</div>
      ${footer ? `<div class="modal-foot">${footer}</div>` : ''}
    </div>`
  document.body.appendChild(overlay)
  let closed = false
  const close = () => {
    if (closed) return
    closed = true
    overlay.remove()
    document.removeEventListener('keydown', onKey)
    if (onClose) onClose()
  }
  const onKey = (event) => {
    if (event.key === 'Escape') close()
  }
  document.addEventListener('keydown', onKey)
  overlay.addEventListener('click', (event) => {
    if (event.target === overlay || event.target.closest('[data-close]')) close()
  })
  return { overlay, close, root: overlay.querySelector('.modal') }
}

function confirmDialog({ title, message, confirmLabel = 'Confirm', danger = false }) {
  return new Promise((resolve) => {
    let settled = false
    const finish = (value) => {
      if (settled) return
      settled = true
      resolve(value)
    }
    const { overlay, close } = openModal({
      title,
      body: `<p class="muted" style="margin:0">${esc(message)}</p>`,
      footer: `<button type="button" class="btn ghost" data-cancel>Cancel</button>
               <button type="button" class="btn ${danger ? 'danger' : 'primary'}" data-confirm>${esc(confirmLabel)}</button>`,
      onClose: () => finish(false),
    })
    overlay.querySelector('[data-cancel]').addEventListener('click', () => {
      finish(false)
      close()
    })
    overlay.querySelector('[data-confirm]').addEventListener('click', () => {
      finish(true)
      close()
    })
  })
}

// ---------------------------------------------------------------------------
// boot / shell
// ---------------------------------------------------------------------------

async function boot() {
  if (!state.token) return renderLogin()
  try {
    state.me = await api('/me')
    state.route = routeFromHash()
    render()
  } catch (error) {
    if (error.status !== 401) {
      state.token = ''
      localStorage.removeItem(TOKEN_KEY)
      renderLogin(`Could not verify the session: ${error.message}`)
    }
  }
}

function renderLogin(message = '') {
  app.className = ''
  app.innerHTML = `
    <div class="login">
      <section class="login-hero">
        <div class="brand">
          <div class="brand-mark">M</div>
          <div class="brand-text"><strong>MeiSync</strong><span>Admin console</span></div>
        </div>
        <div>
          <h1>One file.<br />One <span>YDoc</span>.<br />Every protocol.</h1>
          <p class="lede">Operate the node behind WebDAV, S3 and YSync. Inspect accounts, vaults, storage and the activity trail from one place.</p>
          <div class="hero-points">
            <div class="hero-point">${icon('shield')}<span>Role-gated administration with a complete audit trail.</span></div>
            <div class="hero-point">${icon('database')}<span>Live storage, document cache and compaction visibility.</span></div>
            <div class="hero-point">${icon('monitor')}<span>See every Obsidian vault and device connected to the node.</span></div>
          </div>
        </div>
        <div class="foot">MeiSync · WebDAV + S3 + Yjs on SQLite</div>
      </section>
      <section class="login-panel">
        <div class="login-card">
          <h2>Sign in</h2>
          <p class="sub">Use an account with the <span class="chip accent">admin</span> role.</p>
          ${message ? `<div class="alert" style="margin-bottom:16px">${esc(message)}</div>` : ''}
          <div id="login-error"></div>
          <form id="login-form">
            <div class="field">
              <label for="login-user">Username</label>
              <input id="login-user" class="input" autocomplete="username" placeholder="admin" required autofocus />
            </div>
            <div class="field">
              <label for="login-password">Password</label>
              <input id="login-password" class="input" type="password" autocomplete="current-password" placeholder="••••••••" required />
            </div>
            <button class="btn primary" type="submit" id="login-submit">${icon('shield')} Sign in</button>
          </form>
          <p class="hint-line">By default, the first account registered on a fresh node becomes an administrator. You can also pin one with <code>MEISYNC_ADMIN_USER</code> and <code>MEISYNC_ADMIN_PASSWORD</code>.</p>
        </div>
      </section>
    </div>`

  const form = document.getElementById('login-form')
  form.addEventListener('submit', async (event) => {
    event.preventDefault()
    const button = document.getElementById('login-submit')
    const errorBox = document.getElementById('login-error')
    const username = document.getElementById('login-user').value.trim()
    const password = document.getElementById('login-password').value
    errorBox.innerHTML = ''
    button.disabled = true
    button.textContent = 'Signing in…'
    try {
      const result = await api('/login', {
        method: 'POST',
        auth: false,
        body: { username, password },
      })
      state.token = result.token
      state.me = { user: result.user, role: result.role }
      localStorage.setItem(TOKEN_KEY, result.token)
      toast(`Signed in as ${result.user.username}`, 'success')
      render()
    } catch (error) {
      errorBox.innerHTML = `<div class="alert" style="margin-bottom:16px">${esc(error.message)}</div>`
    } finally {
      button.disabled = false
      button.innerHTML = `${icon('shield')} Sign in`
    }
  })
}

function render() {
  state.route = routeFromHash()
  const route = ROUTES[state.route]
  const username = state.me?.user?.username || 'admin'
  const version = state.version
  app.className = ''
  app.innerHTML = `
    <div class="shell">
      <aside class="sidebar">
        <div class="brand">
          <div class="brand-mark">M</div>
          <div class="brand-text"><strong>MeiSync</strong><span>Admin console</span></div>
        </div>
        <div class="nav-label">Manage</div>
        ${Object.entries(ROUTES)
          .map(
            ([id, item]) => `
            <a class="nav-item ${state.route === id ? 'active' : ''}" href="#/${id}">
              ${icon(item.icon)}<span>${esc(item.title)}</span>
            </a>`,
          )
          .join('')}
        <div class="sidebar-footer">
          <div>MeiSync <span class="mono" id="sidebar-version">${esc(version)}</span></div>
          <div>one file = one YDoc</div>
        </div>
      </aside>
      <div class="main">
        <header class="topbar">
          <div class="topbar-title">
            <span class="topbar-brand">MeiSync</span>
            <span class="topbar-sep">/</span>
            <span class="topbar-route">${esc(route.title)}</span>
          </div>
          <div class="spacer"></div>
          <span class="status-pill online"><span class="dot"></span> Online</span>
          <button class="icon-btn" id="refresh-view" title="Refresh">${icon('refresh')}</button>
          <div class="user-chip">
            <span class="avatar">${esc(initials(username))}</span>
            <span class="name">${esc(username)}</span>
            <button class="icon-btn" id="logout" title="Sign out" style="width:26px;height:26px;border:0;background:transparent">${icon('logout', 15)}</button>
          </div>
        </header>
        <main class="view" id="view"></main>
      </div>
    </div>`

  document.getElementById('logout').addEventListener('click', () => logout())
  document.getElementById('refresh-view').addEventListener('click', () => renderView(true))
  renderView()
}

async function renderView(showToast = false) {
  const view = document.getElementById('view')
  const route = state.route
  const renderers = {
    overview: viewOverview,
    users: viewUsers,
    files: viewFiles,
    sessions: viewSessions,
    devices: viewDevices,
    audit: viewAudit,
    settings: viewSettings,
  }
  try {
    await renderers[route](view)
    if (showToast) toast('Refreshed', 'success')
  } catch (error) {
    if (error.status === 401) return
    if (state.route !== route) return
    if (route === 'files') {
      view.innerHTML = `${pageHead('Files', 'Browse every namespace in the node.')}
        <div class="card"><div class="empty">
          <div class="empty-icon" style="color:var(--red)">${icon('x')}</div>
          <strong>Could not open this path</strong>
          <span class="muted-2">${esc(error.message)}</span>
          <button class="btn" id="reset-files-path" type="button">Go to /private</button>
        </div></div>`
      const reset = document.getElementById('reset-files-path')
      if (reset) {
        reset.addEventListener('click', () => {
          state.filePath = '/private'
          state.fileOwner = ''
          localStorage.setItem(FILE_PATH_KEY, state.filePath)
          renderView()
        })
      }
      return
    }
    view.innerHTML = errorState(error.message)
  }
}

// ---------------------------------------------------------------------------
// shared render fragments
// ---------------------------------------------------------------------------

function skeleton(label = 'Loading…') {
  return `<div class="card"><div class="empty"><div class="empty-icon">${icon('refresh')}</div><strong>${esc(label)}</strong><span class="muted-2">Fetching data from the node</span></div></div>`
}

function errorState(message) {
  return `<div class="card"><div class="empty"><div class="empty-icon" style="color:var(--red)">${icon('x')}</div><strong>Could not load this view</strong><span class="muted-2">${esc(message)}</span></div></div>`
}

function emptyState(title, hint, iconName = 'folder') {
  return `<div class="empty"><div class="empty-icon">${icon(iconName)}</div><strong>${esc(title)}</strong><span class="muted-2">${esc(hint)}</span></div>`
}

function pageHead(title, sub, actions = '') {
  return `<div class="page-head">
      <div><h2>${esc(title)}</h2><p>${esc(sub)}</p></div>
      ${actions ? `<div class="actions">${actions}</div>` : ''}
    </div>`
}

function statCard(label, value, hint, tone = '', iconName = 'activity') {
  return `<div class="stat ${tone}">
      <div class="label">${icon(iconName, 14)} ${esc(label)}</div>
      <div class="value">${esc(value)}</div>
      <div class="hint">${esc(hint)}</div>
    </div>`
}

function actionBadge(action) {
  const value = String(action || '')
  let tone = 'blue'
  if (value.includes('failed') || value.includes('denied') || value.includes('delete')) tone = 'red'
  else if (value.includes('create') || value.includes('register') || value.includes('bootstrap')) tone = 'green'
  else if (value.includes('update') || value.includes('revoke')) tone = 'yellow'
  else if (value.includes('sync') || value.includes('device')) tone = 'purple'
  return `<span class="badge ${tone}">${esc(value)}</span>`
}

function auditRows(entries) {
  if (!entries.length) {
    return `<tr><td colspan="5">${emptyState('No activity yet', 'Actions performed by administrators and sync clients will appear here.', 'list')}</td></tr>`
  }
  return entries
    .map(
      (entry) => `<tr>
        <td class="mono muted" style="white-space:nowrap">${esc(fmtDate(entry.at))}</td>
        <td>${esc(entry.actor_name || 'system')}</td>
        <td>${actionBadge(entry.action)}</td>
        <td class="mono truncate">${esc(entry.target || '—')}</td>
        <td class="muted truncate">${esc(entry.detail || '—')}</td>
      </tr>`,
    )
    .join('')
}

// ---------------------------------------------------------------------------
// overview
// ---------------------------------------------------------------------------

async function viewOverview(view) {
  view.innerHTML = skeleton('Loading overview…')
  const data = await api('/overview')
  state.overview = data
  if (state.route !== 'overview') return
  const stats = data.stats
  const documents = data.documents
  const server = data.server
  const disk = data.disk
  const audit = data.recent_audit || []
  state.version = server.version
  const sidebarVersion = document.getElementById('sidebar-version')
  if (sidebarVersion) sidebarVersion.textContent = server.version
  view.innerHTML = `
    ${pageHead('Overview', 'A live picture of this MeiSync node.', `<button class="btn" id="overview-refresh">${icon('refresh')} Refresh</button>`)}
    <div class="grid stat-grid">
      ${statCard('Users', String(stats.users), `${stats.admins} admin · ${stats.disabled_users} disabled`, 'accent', 'users')}
      ${statCard('Files', String(stats.files), `${stats.collections} collections · ${stats.ydocs} YDocs`, '', 'file')}
      ${statCard('Content', fmtBytes(stats.bytes), 'materialised file bytes', 'purple', 'database')}
      ${statCard('Active sessions', String(stats.active_sessions), 'unexpired bearer tokens', '', 'key')}
      ${statCard('Sync devices', String(stats.devices), 'Obsidian vaults registered', 'green', 'monitor')}
      ${statCard('Open documents', String(documents.cached_documents), `${documents.rooms} live YSync rooms`, '', 'activity')}
    </div>

    <div class="split" style="margin-top:16px">
      <div class="card">
        <div class="card-head">
          <h3>Recent activity</h3>
          <span class="badge">last ${audit.length}</span>
          <div class="actions"><a class="btn small ghost" href="#/audit">View all ${icon('chevron', 13)}</a></div>
        </div>
        <div class="table-wrap" style="max-height:420px">
          <table>
            <thead><tr><th>When</th><th>Actor</th><th>Action</th><th>Target</th><th>Detail</th></tr></thead>
            <tbody>${auditRows(audit)}</tbody>
          </table>
        </div>
      </div>
      <div class="stack">
        <div class="card">
          <div class="card-head"><h3>Server</h3><span class="badge green">healthy</span></div>
          <div class="card-body">
            <dl class="kv">
              <dt>Version</dt><dd>${esc(server.version)}</dd>
              <dt>Bind</dt><dd>${esc(server.bind)}</dd>
              <dt>Data dir</dt><dd>${esc(server.data_dir)}</dd>
              <dt>Token TTL</dt><dd>${esc(Math.round(server.token_ttl_secs / 86400))} days</dd>
              <dt>Body limit</dt><dd>${esc(fmtBytes(server.max_body_bytes))}</dd>
              <dt>Public write</dt><dd>${server.public_default_write ? 'enabled' : 'owner / ACL only'}</dd>
            </dl>
          </div>
        </div>
        <div class="card">
          <div class="card-head"><h3>Storage</h3><span class="badge purple">${esc(fmtBytes(disk.data_dir_bytes))}</span></div>
          <div class="card-body">
            <dl class="kv">
              <dt>YDoc store</dt><dd>${esc(fmtBytes(disk.documents_bytes))}</dd>
              <dt>SQLite</dt><dd>${esc(fmtBytes(disk.database_bytes))}</dd>
              <dt>Total data dir</dt><dd>${esc(fmtBytes(disk.data_dir_bytes))}</dd>
              <dt>Compaction</dt><dd>${esc(server.compaction.max_updates)} updates / ${esc(fmtBytes(server.compaction.max_update_bytes))}</dd>
            </dl>
          </div>
        </div>
        <div class="card">
          <div class="card-head"><h3>Capabilities</h3></div>
          <div class="card-body">
            <div class="chip-row">${(data.capabilities || []).map((item) => `<span class="chip">${esc(item)}</span>`).join('')}</div>
          </div>
        </div>
      </div>
    </div>`
  document.getElementById('overview-refresh').addEventListener('click', () => renderView(true))
}

// ---------------------------------------------------------------------------
// users
// ---------------------------------------------------------------------------

async function viewUsers(view) {
  view.innerHTML = skeleton('Loading users…')
  const data = await api(`/users?limit=500&search=${encodeURIComponent(state.userSearch)}`)
  if (state.route !== 'users') return
  state.users = data.users
  const rows = data.users.length
    ? data.users
        .map(
          (user) => `<tr>
            <td>
              <div class="user-cell">
                <span class="avatar-sm">${esc(initials(user.username))}</span>
                <div>
                  <div style="font-weight:600">${esc(user.username)}${state.me?.user?.id === user.id ? ' <span class="muted-2">(you)</span>' : ''}</div>
                  <div class="mono muted-2" style="font-size:10px">${esc(user.id)}</div>
                </div>
              </div>
            </td>
            <td><span class="badge ${user.role === 'admin' ? 'purple' : 'blue'}">${esc(user.role)}</span></td>
            <td>${user.disabled ? '<span class="badge red"><span class="dot"></span> disabled</span>' : '<span class="badge green"><span class="dot"></span> active</span>'}</td>
            <td class="mono">${user.file_count}</td>
            <td class="mono">${esc(fmtBytes(user.bytes))}</td>
            <td class="mono">${user.session_count}</td>
            <td class="muted" style="white-space:nowrap">${esc(fmtRelative(user.last_login_at))}</td>
            <td>
              <div class="row-actions">
                <button class="btn small ghost" data-edit="${esc(user.id)}">${icon('edit', 13)} Edit</button>
                <button class="btn small danger" data-delete="${esc(user.id)}" data-name="${esc(user.username)}">${icon('trash', 13)}</button>
              </div>
            </td>
          </tr>`,
        )
        .join('')
    : `<tr><td colspan="8">${emptyState('No users match', 'Try a different search, or create an account.', 'users')}</td></tr>`

  view.innerHTML = `
    ${pageHead(
      'Users',
      `${data.total} account${data.total === 1 ? '' : 's'} in this node.`,
      `<button class="btn primary" id="create-user">${icon('plus')} New user</button>`,
    )}
    <div class="card">
      <div class="card-head">
        <form class="searchbar" id="user-search-form">
          <div style="position:relative">
            <span style="position:absolute;left:10px;top:9px;color:var(--muted-2)">${icon('search', 15)}</span>
            <input id="user-search" class="input" style="padding-left:32px" placeholder="Search username…" value="${esc(state.userSearch)}" />
          </div>
          <button class="btn" type="submit">Search</button>
          ${state.userSearch ? '<button class="btn ghost" type="button" id="clear-user-search">Clear</button>' : ''}
        </form>
        <div class="actions"><span class="badge">${data.users.length} shown</span></div>
      </div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>User</th><th>Role</th><th>Status</th><th>Files</th><th>Content</th><th>Sessions</th><th>Last login</th><th class="right">Actions</th></tr></thead>
          <tbody>${rows}</tbody>
        </table>
      </div>
    </div>`

  document.getElementById('create-user').addEventListener('click', () => openUserModal(null))
  document.getElementById('user-search-form').addEventListener('submit', (event) => {
    event.preventDefault()
    state.userSearch = document.getElementById('user-search').value.trim()
    renderView()
  })
  const clear = document.getElementById('clear-user-search')
  if (clear) {
    clear.addEventListener('click', () => {
      state.userSearch = ''
      renderView()
    })
  }
  view.querySelectorAll('[data-edit]').forEach((button) => {
    button.addEventListener('click', () => {
      const user = state.users.find((item) => item.id === button.dataset.edit)
      if (user) openUserModal(user)
    })
  })
  view.querySelectorAll('[data-delete]').forEach((button) => {
    button.addEventListener('click', async () => {
      const id = button.dataset.delete
      const name = button.dataset.name
      const confirmed = await confirmDialog({
        title: 'Delete user?',
        message: `Delete “${name}” together with /private/${name}? This cannot be undone. Their public files and ACL grants are kept.`,
        confirmLabel: 'Delete user',
        danger: true,
      })
      if (!confirmed) return
      try {
        await api(`/users/${encodeURIComponent(id)}?purge=true`, { method: 'DELETE' })
        toast(`Deleted ${name}`, 'success')
        renderView()
      } catch (error) {
        toast(error.message, 'error')
      }
    })
  })
}

function openUserModal(user) {
  const editing = !!user
  const formId = `user-form-${Date.now()}`
  const body = `
    <form id="${formId}" class="form-grid">
      <div class="field span-2">
        <label for="u-username">Username</label>
        <input id="u-username" class="input" required ${editing ? 'disabled' : ''} value="${esc(user?.username || '')}" placeholder="alice" autocomplete="off" />
        ${editing ? '<span class="help">The namespace /private/&lt;username&gt; is tied to this name and cannot be renamed here.</span>' : '<span class="help">1–64 characters: letters, digits, dot, dash, underscore.</span>'}
      </div>
      <div class="field span-2">
        <label for="u-password">${editing ? 'New password (optional)' : 'Password'}</label>
        <input id="u-password" class="input" type="password" ${editing ? '' : 'required'} placeholder="${editing ? 'Leave blank to keep the current password' : 'At least 6 characters'}" autocomplete="new-password" />
        ${editing ? '<span class="help">A password reset revokes every active session of the account.</span>' : ''}
      </div>
      <div class="field">
        <label for="u-role">Role</label>
        <select id="u-role" class="select">
          <option value="user" ${user?.role === 'user' ? 'selected' : ''}>user</option>
          <option value="admin" ${user?.role === 'admin' ? 'selected' : ''}>admin</option>
        </select>
      </div>
      <div class="field">
        <label>Status</label>
        <label class="checkbox-row"><input type="checkbox" id="u-disabled" ${user?.disabled ? 'checked' : ''} /> Disabled</label>
      </div>
    </form>`
  const footer = `
    <button type="button" class="btn ghost" data-close>Cancel</button>
    <button type="submit" form="${formId}" class="btn primary">${editing ? 'Save changes' : 'Create user'}</button>`
  const { overlay, close } = openModal({
    title: editing ? `Edit ${user.username}` : 'New user',
    body,
    footer,
  })
  const form = overlay.querySelector(`#${formId}`)
  form.addEventListener('submit', async (event) => {
    event.preventDefault()
    const role = overlay.querySelector('#u-role').value
    const disabled = overlay.querySelector('#u-disabled').checked
    const password = overlay.querySelector('#u-password').value
    const submit = overlay.querySelector('.modal-foot .primary')
    submit.disabled = true
    try {
      if (editing) {
        const payload = { role, disabled }
        if (password) payload.password = password
        await api(`/users/${encodeURIComponent(user.id)}`, { method: 'PATCH', body: payload })
        toast(`Updated ${user.username}`, 'success')
      } else {
        const username = overlay.querySelector('#u-username').value.trim()
        await api('/users', { method: 'POST', body: { username, password, role, disabled } })
        toast(`Created ${username}`, 'success')
      }
      close()
      renderView()
    } catch (error) {
      toast(error.message, 'error')
    } finally {
      submit.disabled = false
    }
  })
}

// ---------------------------------------------------------------------------
// files
// ---------------------------------------------------------------------------

async function viewFiles(view) {
  view.innerHTML = skeleton('Loading namespace…')
  const params = new URLSearchParams({ path: state.filePath, depth: '1' })
  if (state.fileOwner.trim()) params.set('owner', state.fileOwner.trim())
  const data = await api(`/files?${params}`)
  if (state.route !== 'files') return
  const owners = data.owners || {}
  const current = data.path || state.filePath
  const rowsData = data.files.filter((file) => file.path !== current)
  const sorted = [...rowsData].sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === 'collection' ? -1 : 1
    return a.name.localeCompare(b.name)
  })
  const crumbs = breadcrumb(current)
  const rows = sorted.length
    ? sorted
        .map(
          (file) => `<tr>
            <td>
              <div class="user-cell">
                <span class="file-icon ${file.kind === 'collection' ? 'dir' : ''}">${icon(file.kind === 'collection' ? 'folder' : 'file')}</span>
                <button class="file-name" data-open="${esc(file.path)}" data-kind="${esc(file.kind)}">${esc(file.name)}</button>
              </div>
            </td>
            <td><span class="badge ${file.kind === 'collection' ? 'blue' : ''}">${esc(file.kind)}</span></td>
            <td class="mono">${file.kind === 'collection' ? '—' : esc(fmtBytes(file.size))}</td>
            <td class="muted">${esc(owners[file.owner_id] || file.owner_id.slice(0, 8))}</td>
            <td><span class="badge ${file.visibility === 'public' ? 'green' : ''}">${esc(file.visibility)}</span></td>
            <td class="muted" style="white-space:nowrap">${esc(fmtRelative(file.updated_at))}</td>
            <td>
              <div class="row-actions">
                ${file.kind === 'file' ? `<button class="btn small ghost" data-preview="${esc(file.path)}">${icon('eye', 13)} View</button>` : `<button class="btn small ghost" data-open="${esc(file.path)}" data-kind="collection">${icon('folder', 13)} Open</button>`}
                <button class="btn small danger" data-remove="${esc(file.path)}" data-name="${esc(file.name)}" data-kind="${esc(file.kind)}">${icon('trash', 13)}</button>
              </div>
            </td>
          </tr>`,
        )
        .join('')
    : `<tr><td colspan="7">${emptyState('This collection is empty', 'Upload files through WebDAV, S3, the sync API or the Obsidian plugin.', 'folder')}</td></tr>`

  view.innerHTML = `
    ${pageHead(
      'Files',
      'Every protocol writes into the same namespace; this is a read-only browser with administrative delete.',
      `<button class="btn" id="files-up">${icon('upload', 14)} Up</button><button class="btn" id="files-refresh">${icon('refresh')} Refresh</button>`,
    )}
    <div class="card">
      <div class="pathbar">
        <div class="breadcrumb">${crumbs}</div>
        <div class="spacer"></div>
        <form class="searchbar" id="files-filter">
          <input id="files-owner" class="input" style="width:170px" placeholder="Owner (username)" value="${esc(state.fileOwner)}" />
          <input id="files-path" class="input" style="width:290px" placeholder="/private/alice" value="${esc(current)}" />
          <button class="btn" type="submit">${icon('search', 14)} Go</button>
        </form>
      </div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>Name</th><th>Kind</th><th>Size</th><th>Owner</th><th>Visibility</th><th>Updated</th><th class="right">Actions</th></tr></thead>
          <tbody>${rows}</tbody>
        </table>
      </div>
    </div>`

  const setPath = (path) => {
    state.filePath = path || '/'
    localStorage.setItem(FILE_PATH_KEY, state.filePath)
    renderView()
  }
  document.getElementById('files-up').addEventListener('click', () => {
    const parts = current.split('/').filter(Boolean)
    parts.pop()
    setPath(`/${parts.join('/')}` || '/')
  })
  document.getElementById('files-refresh').addEventListener('click', () => renderView(true))
  document.getElementById('files-filter').addEventListener('submit', (event) => {
    event.preventDefault()
    state.filePath = document.getElementById('files-path').value.trim() || '/'
    state.fileOwner = document.getElementById('files-owner').value.trim()
    localStorage.setItem(FILE_PATH_KEY, state.filePath)
    renderView()
  })
  view.querySelectorAll('[data-open]').forEach((button) => {
    button.addEventListener('click', () => {
      const path = button.dataset.open
      if (button.dataset.kind === 'collection') setPath(path)
      else openPreview(path, button.textContent.trim(), '')
    })
  })
  view.querySelectorAll('[data-preview]').forEach((button) => {
    button.addEventListener('click', () => {
      const path = button.dataset.preview
      const file = data.files.find((item) => item.path === path)
      openPreview(path, file?.name || path.split('/').pop(), file?.content_type || '')
    })
  })
  view.querySelectorAll('[data-remove]').forEach((button) => {
    button.addEventListener('click', async () => {
      const path = button.dataset.remove
      const kind = button.dataset.kind
      const confirmed = await confirmDialog({
        title: kind === 'collection' ? 'Delete collection?' : 'Delete file?',
        message:
          kind === 'collection'
            ? `“${path}” and everything below it will be removed from every protocol. This cannot be undone.`
            : `“${path}” will be removed from every protocol. This cannot be undone.`,
        confirmLabel: 'Delete',
        danger: true,
      })
      if (!confirmed) return
      try {
        await api(`/files?path=${encodeURIComponent(path)}`, { method: 'DELETE' })
        toast('Deleted', 'success')
        renderView()
      } catch (error) {
        toast(error.message, 'error')
      }
    })
  })
}

function breadcrumb(path) {
  const parts = String(path || '/').split('/').filter(Boolean)
  let walked = ''
  const items = [`<button type="button" data-crumb="/">/</button>`]
  parts.forEach((part, index) => {
    walked += `/${part}`
    items.push('<span class="sep">›</span>')
    items.push(
      `<button type="button" data-crumb="${esc(walked)}" ${index === parts.length - 1 ? 'style="color:var(--text)"' : ''}>${esc(part)}</button>`,
    )
  })
  // Attach the breadcrumb navigation after the HTML is inserted.
  window.setTimeout(() => {
    document.querySelectorAll('[data-crumb]').forEach((button) => {
      button.addEventListener('click', () => {
        state.filePath = button.dataset.crumb
        localStorage.setItem(FILE_PATH_KEY, state.filePath)
        renderView()
      })
    })
  }, 0)
  return items.join('')
}

async function openPreview(path, name, contentType) {
  const bodyId = `preview-${Date.now()}`
  let objectUrl = null
  const { overlay } = openModal({
    title: name || path,
    wide: true,
    body: `<div id="${bodyId}" class="empty"><div class="empty-icon">${icon('refresh')}</div><strong>Loading preview…</strong><span class="muted-2">${esc(path)}</span></div>`,
    footer: `<a class="btn ghost" data-download href="#" download="${esc(name || 'file')}">${icon('download')} Download</a><button type="button" class="btn" data-close>Close</button>`,
    onClose: () => {
      if (objectUrl) URL.revokeObjectURL(objectUrl)
    },
  })
  const target = overlay.querySelector(`#${bodyId}`)
  try {
    const response = await api(`/file?path=${encodeURIComponent(path)}`, { raw: true })
    const blob = await response.blob()
    const type = response.headers.get('content-type') || contentType || blob.type || ''
    const download = overlay.querySelector('[data-download]')
    objectUrl = URL.createObjectURL(blob)
    if (!overlay.isConnected) {
      URL.revokeObjectURL(objectUrl)
      objectUrl = null
      return
    }
    download.href = objectUrl
    if (isTextual(type, name)) {
      const text = await blob.text()
      const truncated = text.length > 400_000
      target.outerHTML = `<pre class="preview">${esc(truncated ? `${text.slice(0, 400_000)}\n\n… preview truncated at 400 KB` : text)}</pre>
        <div class="chip-row" style="margin-top:12px"><span class="chip">${esc(type || 'text/plain')}</span><span class="chip">${esc(fmtBytes(blob.size))}</span></div>`
    } else {
      target.outerHTML = `<div class="empty"><div class="empty-icon">${icon('file')}</div><strong>Binary file</strong><span class="muted-2">${esc(type || 'application/octet-stream')} · ${esc(fmtBytes(blob.size))}</span><p class="muted">Use Download to inspect it locally.</p></div>`
    }
  } catch (error) {
    target.innerHTML = `<div class="alert">${esc(error.message)}</div>`
  }
}

function isTextual(contentType, name) {
  const type = String(contentType || '').toLowerCase()
  if (type.startsWith('text/')) return true
  if (/(json|xml|yaml|javascript|typescript|x-sh|toml|csv|markdown)/.test(type)) return true
  return /\.(md|markdown|txt|json|ya?ml|toml|xml|csv|ts|tsx|js|jsx|css|html|rs|go|py|sh|log|canvas)$/i.test(name || '')
}

// ---------------------------------------------------------------------------
// sessions / devices / audit
// ---------------------------------------------------------------------------

async function viewSessions(view) {
  view.innerHTML = skeleton('Loading sessions…')
  const data = await api('/sessions?limit=500')
  if (state.route !== 'sessions') return
  state.sessions = data.sessions
  const rows = data.sessions.length
    ? data.sessions
        .map(
          (session) => `<tr>
            <td><div class="user-cell"><span class="avatar-sm">${esc(initials(session.username))}</span><span>${esc(session.username)}</span></div></td>
            <td class="mono muted truncate">${esc(session.token_prefix)}</td>
            <td class="muted" style="white-space:nowrap">${esc(fmtDate(session.created_at))}</td>
            <td class="muted" style="white-space:nowrap">${esc(fmtDate(session.expires_at))}</td>
            <td><div class="row-actions"><button class="btn small danger" data-revoke="${esc(session.id)}" data-name="${esc(session.username)}">${icon('x', 13)} Revoke</button></div></td>
          </tr>`,
        )
        .join('')
    : `<tr><td colspan="5">${emptyState('No active sessions', 'Tokens appear here after a client signs in.', 'key')}</td></tr>`

  view.innerHTML = `
    ${pageHead('Sessions', 'Every unexpired session. Revoking one logs that client out immediately.', `<button class="btn" id="sessions-refresh">${icon('refresh')} Refresh</button>`)}
    <div class="card">
      <div class="card-head"><h3>Active tokens</h3><span class="badge">${data.sessions.length}</span></div>
      <div class="table-wrap">
        <table><thead><tr><th>User</th><th>Token</th><th>Created</th><th>Expires</th><th class="right">Actions</th></tr></thead><tbody>${rows}</tbody></table>
      </div>
    </div>`
  document.getElementById('sessions-refresh').addEventListener('click', () => renderView(true))
  view.querySelectorAll('[data-revoke]').forEach((button) => {
    button.addEventListener('click', async () => {
      const id = button.dataset.revoke
      const confirmed = await confirmDialog({
        title: 'Revoke session?',
        message: `The client using this session (${button.dataset.name}) will have to sign in again.`,
        confirmLabel: 'Revoke',
        danger: true,
      })
      if (!confirmed) return
      try {
        await api(`/sessions/${encodeURIComponent(id)}`, { method: 'DELETE' })
        toast('Session revoked', 'success')
        renderView()
      } catch (error) {
        toast(error.message, 'error')
      }
    })
  })
}

async function viewDevices(view) {
  view.innerHTML = skeleton('Loading devices…')
  const data = await api('/devices?limit=500')
  if (state.route !== 'devices') return
  state.devices = data.devices
  const rows = data.devices.length
    ? data.devices
        .map(
          (device) => `<tr>
            <td><div class="user-cell"><span class="avatar-sm">${esc(initials(device.name || '?'))}</span><div><div style="font-weight:600">${esc(device.name || 'Obsidian')}</div><div class="mono muted-2" style="font-size:10px">${esc(device.device_id)}</div></div></div></td>
            <td>${esc(device.username || device.user_id)}</td>
            <td class="mono truncate" title="${esc(device.vault)}">${esc(device.vault)}</td>
            <td><span class="badge">${esc(device.platform || 'unknown')}</span> <span class="muted mono">${esc(device.plugin_version || '')}</span></td>
            <td class="muted" style="white-space:nowrap" title="${esc(fmtDate(device.last_seen_at))}">${esc(fmtRelative(device.last_seen_at))}</td>
            <td><div class="row-actions"><button class="btn small danger" data-remove-device="${esc(device.user_id)}" data-device="${esc(device.device_id)}">${icon('trash', 13)} Forget</button></div></td>
          </tr>`,
        )
        .join('')
    : `<tr><td colspan="6">${emptyState('No sync devices yet', 'Connect the MeiSync Obsidian plugin and register a vault to see it here.', 'monitor')}</td></tr>`

  view.innerHTML = `
    ${pageHead('Sync devices', 'Obsidian vaults that completed a handshake with /api/sync.', `<button class="btn" id="devices-refresh">${icon('refresh')} Refresh</button>`)}
    <div class="card">
      <div class="card-head"><h3>Registered devices</h3><span class="badge">${data.devices.length}</span></div>
      <div class="table-wrap">
        <table><thead><tr><th>Device</th><th>User</th><th>Vault</th><th>Client</th><th>Last seen</th><th class="right">Actions</th></tr></thead><tbody>${rows}</tbody></table>
      </div>
    </div>`
  document.getElementById('devices-refresh').addEventListener('click', () => renderView(true))
  view.querySelectorAll('[data-remove-device]').forEach((button) => {
    button.addEventListener('click', async () => {
      const confirmed = await confirmDialog({
        title: 'Forget device?',
        message: 'The device registration is removed. The client can register again on its next sync; files are not touched.',
        confirmLabel: 'Forget',
        danger: true,
      })
      if (!confirmed) return
      try {
        await api(`/devices/${encodeURIComponent(button.dataset.removeDevice)}/${encodeURIComponent(button.dataset.device)}`, { method: 'DELETE' })
        toast('Device forgotten', 'success')
        renderView()
      } catch (error) {
        toast(error.message, 'error')
      }
    })
  })
}

async function viewAudit(view) {
  view.innerHTML = skeleton('Loading audit log…')
  const data = await api('/audit?limit=500')
  if (state.route !== 'audit') return
  state.audit = data.entries
  view.innerHTML = `
    ${pageHead('Audit log', 'Administrative and security relevant events, newest first.', `<button class="btn" id="audit-refresh">${icon('refresh')} Refresh</button>`)}
    <div class="card">
      <div class="card-head"><h3>Events</h3><span class="badge">${data.entries.length} shown</span><div class="actions"><input id="audit-filter" class="input" style="width:230px" placeholder="Filter action, actor, target…" /></div></div>
      <div class="table-wrap">
        <table><thead><tr><th>When</th><th>Actor</th><th>Action</th><th>Target</th><th>Detail</th></tr></thead><tbody id="audit-body">${auditRows(data.entries)}</tbody></table>
      </div>
    </div>`
  document.getElementById('audit-refresh').addEventListener('click', () => renderView(true))
  document.getElementById('audit-filter').addEventListener('input', (event) => {
    const needle = event.target.value.trim().toLowerCase()
    const filtered = needle
      ? data.entries.filter((entry) =>
          [entry.action, entry.actor_name, entry.target, entry.detail].filter(Boolean).join(' ').toLowerCase().includes(needle),
        )
      : data.entries
    document.getElementById('audit-body').innerHTML = auditRows(filtered)
  })
}

// ---------------------------------------------------------------------------
// settings
// ---------------------------------------------------------------------------

async function viewSettings(view) {
  if (!state.overview) state.overview = await api('/overview')
  if (state.route !== 'settings') return
  const data = state.overview
  state.version = data.server.version
  const sidebarVersion = document.getElementById('sidebar-version')
  if (sidebarVersion) sidebarVersion.textContent = data.server.version
  const server = data.server
  const disk = data.disk
  const endpoints = [
    ['Admin console', '/admin', 'this UI'],
    ['JSON API', '/api', 'control plane'],
    ['Obsidian sync', '/api/sync/info', 'manifest + content + websocket'],
    ['WebDAV', '/dav', 'full file I/O'],
    ['S3', '/s3', 'path-style + SigV4'],
    ['YSync', '/ysync/<path>', 'Yjs incremental sync'],
    ['Health', '/api/health', 'liveness probe'],
  ]
  view.innerHTML = `
    ${pageHead('Server', 'Read-only runtime configuration. Restart the node with CLI flags or environment variables to change it.', `<button class="btn" id="settings-refresh">${icon('refresh')} Refresh</button>`)}
    <div class="split">
      <div class="stack">
        <div class="card">
          <div class="card-head"><h3>Runtime</h3><span class="badge green">v${esc(server.version)}</span></div>
          <div class="card-body">
            <dl class="kv">
              <dt>Bind address</dt><dd>${esc(server.bind)}</dd>
              <dt>Data directory</dt><dd>${esc(server.data_dir)}</dd>
              <dt>Token lifetime</dt><dd>${esc(Math.round(server.token_ttl_secs / 86400))} days</dd>
              <dt>Max body</dt><dd>${esc(fmtBytes(server.max_body_bytes))}</dd>
              <dt>Public write</dt><dd>${server.public_default_write ? 'any authenticated user' : 'owner / explicit ACL'}</dd>
              <dt>Compaction</dt><dd>${esc(server.compaction.max_updates)} updates · ${esc(fmtBytes(server.compaction.max_update_bytes))} · ${esc(fmtBytes(server.compaction.snapshot_bytes))} snapshot</dd>
            </dl>
          </div>
        </div>
        <div class="card">
          <div class="card-head"><h3>Storage</h3><span class="badge purple">${esc(fmtBytes(disk.data_dir_bytes))}</span></div>
          <div class="card-body">
            <dl class="kv">
              <dt>YDoc store</dt><dd>${esc(fmtBytes(disk.documents_bytes))}</dd>
              <dt>SQLite database</dt><dd>${esc(fmtBytes(disk.database_bytes))}</dd>
              <dt>Data directory</dt><dd>${esc(fmtBytes(disk.data_dir_bytes))}</dd>
              <dt>Collections</dt><dd>${esc(data.stats.collections)}</dd>
              <dt>YDocs</dt><dd>${esc(data.stats.ydocs)}</dd>
            </dl>
          </div>
        </div>
        <div class="card">
          <div class="card-head"><h3>Capabilities</h3></div>
          <div class="card-body"><div class="chip-row">${(data.capabilities || []).map((item) => `<span class="chip accent">${esc(item)}</span>`).join('')}</div></div>
        </div>
      </div>
      <div class="stack">
        <div class="card">
          <div class="card-head"><h3>Endpoints</h3><span class="sub">same origin</span></div>
          <div class="table-wrap" style="max-height:none">
            <table><thead><tr><th>Surface</th><th>Path</th><th class="right"></th></tr></thead><tbody>
              ${endpoints
                .map(
                  ([name, path, note]) => `<tr>
                    <td><div style="font-weight:600">${esc(name)}</div><div class="muted-2" style="font-size:11px">${esc(note)}</div></td>
                    <td class="mono">${esc(path)}</td>
                    <td class="right"><button class="btn small ghost" data-open-endpoint="${esc(path.split('<')[0])}">${icon('external', 13)} Open</button></td>
                  </tr>`,
                )
                .join('')}
            </tbody></table>
          </div>
        </div>
        <div class="card">
          <div class="card-head"><h3>Client setup</h3></div>
          <div class="card-body" style="display:grid;gap:12px">
            <div class="alert info">The Obsidian plugin talks to <span class="mono">/api/sync</span> and the WebSocket feed at <span class="mono">/api/sync/ws</span>. Use this node's origin as the server URL in the plugin settings.</div>
            <div class="field">
              <label>Server URL</label>
              <div class="searchbar"><input class="input" readonly value="${esc(location.origin)}" id="server-url" /><button class="btn" id="copy-server-url">Copy</button></div>
            </div>
            <div class="field">
              <label>Admin bootstrap</label>
              <div class="help">Set <code class="chip">MEISYNC_ADMIN_USER</code> and <code class="chip">MEISYNC_ADMIN_PASSWORD</code> before the first start, or let the first registered account become administrator.</div>
            </div>
          </div>
        </div>
      </div>
    </div>`
  document.getElementById('settings-refresh').addEventListener('click', async () => {
    state.overview = null
    await renderView()
    toast('Refreshed', 'success')
  })
  view.querySelectorAll('[data-open-endpoint]').forEach((button) => {
    button.addEventListener('click', () => {
      const path = button.dataset.openEndpoint
      if (path.includes('<')) return
      window.open(path, '_blank', 'noopener')
    })
  })
  const copy = document.getElementById('copy-server-url')
  copy.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(location.origin)
      toast('Server URL copied', 'success')
    } catch {
      toast('Could not access the clipboard', 'error')
    }
  })
}

// ---------------------------------------------------------------------------
// start
// ---------------------------------------------------------------------------

window.addEventListener('hashchange', () => {
  if (!state.me) return
  if (state.route === routeFromHash()) return
  state.route = routeFromHash()
  state.overview = state.route === 'settings' ? state.overview : state.overview
  render()
})

boot()
