import { useCallback, useEffect, useMemo, useState } from 'react'
import { ApiError, MeiSyncApi, type AuthToken, type PublicUser } from './meisync/api'
import { Workspace } from './Workspace'

const TOKEN_KEY = 'meisync.token'
const PATH_KEY = 'meisync.path'

interface AuthState {
  token: string
  user: PublicUser
}

function loadAuth(): AuthState | null {
  const token = localStorage.getItem(TOKEN_KEY)
  const raw = localStorage.getItem(`${TOKEN_KEY}.user`)
  if (!token || !raw) return null
  try {
    return { token, user: JSON.parse(raw) as PublicUser }
  } catch {
    return null
  }
}

function defaultPath(user: PublicUser): string {
  return `/private/${user.username}/notes/todo.md`
}

export function App() {
  const params = useMemo(() => new URLSearchParams(location.search), [])
  const [auth, setAuth] = useState<AuthState | null>(loadAuth)
  const [path, setPath] = useState<string>(
    () => params.get('path') ?? localStorage.getItem(PATH_KEY) ?? '',
  )
  /** The top bar edits a draft so that typing does not open a new YSync session per keystroke. */
  const [draft, setDraft] = useState(path)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const api = useMemo(() => new MeiSyncApi({ baseUrl: '', token: auth?.token ?? null }), [auth?.token])

  useEffect(() => {
    if (!auth) return
    if (path) return
    const initial = defaultPath(auth.user)
    setPath(initial)
    setDraft(initial)
    localStorage.setItem(PATH_KEY, initial)
  }, [auth, path])

  const acceptAuth = useCallback((result: AuthToken) => {
    localStorage.setItem(TOKEN_KEY, result.token)
    localStorage.setItem(`${TOKEN_KEY}.user`, JSON.stringify(result.user))
    setAuth({ token: result.token, user: result.user })
    setError(null)
  }, [])

  const login = useCallback(
    async (username: string, password: string, mode: 'login' | 'register') => {
      setBusy(true)
      setError(null)
      try {
        const result = mode === 'register' ? await api.register(username, password) : await api.login(username, password)
        acceptAuth(result)
      } catch (err) {
        setError(err instanceof ApiError ? `${err.kind}: ${err.message}` : String(err))
      } finally {
        setBusy(false)
      }
    },
    [api, acceptAuth],
  )

  const logout = useCallback(async () => {
    try {
      await api.logout()
    } catch {
      /* the token may already be gone server side */
    }
    localStorage.removeItem(TOKEN_KEY)
    localStorage.removeItem(`${TOKEN_KEY}.user`)
    setAuth(null)
  }, [api])

  const openPath = useCallback((next: string) => {
    setPath(next)
    setDraft(next)
    localStorage.setItem(PATH_KEY, next)
  }, [])

  const openInNewTab = useCallback(() => {
    const target = new URL(location.href)
    target.searchParams.set('path', path)
    // A distinct `me` seed gives the new tab its own colour in the awareness list.
    target.searchParams.set('me', Math.random().toString(36).slice(2, 8))
    window.open(target.toString(), '_blank', 'noopener')
  }, [path])

  if (!auth) {
    return (
      <LoginScreen busy={busy} error={error} defaultUsername={params.get('user') ?? ''} onSubmit={login} />
    )
  }

  return (
    <div className="app">
      <header className="topbar">
        <h1>MeiSync · Tiptap + Yjs test client</h1>
        <span className="sub">one file = one YDoc</span>
        <div className="spacer" />
        <input
          style={{ width: 340 }}
          placeholder="/private/alice/notes/todo.md"
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'Enter') openPath(draft)
          }}
        />
        <button className="primary" onClick={() => openPath(draft)} disabled={draft === path}>
          Open
        </button>
        <button onClick={openInNewTab} title="Open a second collaborative client for the same file">
          New tab
        </button>
        <span className="sub">
          {auth.user.username} · <span className="mono">{auth.token.slice(0, 8)}…</span>
        </span>
        <button onClick={logout}>Log out</button>
      </header>
      {error ? (
        <div className="banner error" style={{ margin: 10 }}>
          {error}
        </div>
      ) : null}
      <Workspace key={`${auth.token}:${path}`} api={api} auth={auth} path={path} seed={params.get('me') ?? ''} onPathChange={openPath} />
    </div>
  )
}

function LoginScreen({
  busy,
  error,
  defaultUsername,
  onSubmit,
}: {
  busy: boolean
  error: string | null
  defaultUsername: string
  onSubmit: (username: string, password: string, mode: 'login' | 'register') => void
}) {
  const [username, setUsername] = useState(defaultUsername)
  const [password, setPassword] = useState('')
  return (
    <div className="app">
      <header className="topbar">
        <h1>MeiSync · Tiptap + Yjs test client</h1>
      </header>
      <div className="login">
        <div className="section">
          <h2>Account</h2>
          {error ? <div className="banner error">{error}</div> : null}
          <div className="field">
            <label htmlFor="username">username</label>
            <input id="username" value={username} onChange={(event) => setUsername(event.target.value)} autoComplete="username" />
          </div>
          <div className="field">
            <label htmlFor="password">password</label>
            <input
              id="password"
              type="password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              autoComplete="current-password"
              onKeyDown={(event) => {
                if (event.key === 'Enter') onSubmit(username, password, 'login')
              }}
            />
          </div>
          <div className="row">
            <button
              className="primary"
              disabled={busy || !username || !password}
              onClick={() => onSubmit(username, password, 'login')}
            >
              Log in
            </button>
            <button disabled={busy || !username || !password} onClick={() => onSubmit(username, password, 'register')}>
              Register
            </button>
            <span className="muted">registration provisions /private/&lt;user&gt; and bucket &lt;user&gt;</span>
          </div>
        </div>
      </div>
    </div>
  )
}