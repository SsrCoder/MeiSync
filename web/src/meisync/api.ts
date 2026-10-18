/**
 * Control plane client: `/api` (auth, listings, ACL) plus a few raw WebDAV verbs so the test
 * client can drive the *other* side of the server (write a file over HTTP, read the materialised
 * bytes back) and compare them with what the editor sees.
 *
 * All requests are same-origin and go through the Vite dev proxy (see `vite.config.ts`), which
 * also keeps the node free of CORS configuration.
 */

export interface PublicUser {
  id: string
  username: string
  created_at?: string
}

export interface AuthToken {
  token: string
  user: PublicUser
}

export interface FileNode {
  id: string
  parent_id: string | null
  name: string
  path: string
  owner_id: string
  visibility: 'private' | 'public'
  content_type: string
  kind: 'file' | 'collection'
  created_at: string
  updated_at: string
  size: number
  etag: string | null
  version: number
  /** `full` means the bytes live base64 encoded inside the shared text (§5.2). */
  sync_mode: 'incremental' | 'full'
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly kind: string,
    message: string,
  ) {
    super(message)
    this.name = 'ApiError'
  }
}

export interface ApiOptions {
  /** Prefix for `/api` and `/dav`, e.g. `http://127.0.0.1:8080` or `''` (same origin). */
  baseUrl?: string
  token?: string | null
  fetchImpl?: typeof fetch
}

export class MeiSyncApi {
  baseUrl: string
  token: string | null
  private readonly fetchImpl: typeof fetch

  constructor(options: ApiOptions = {}) {
    this.baseUrl = (options.baseUrl ?? '').replace(/\/$/, '')
    this.token = options.token ?? null
    this.fetchImpl = options.fetchImpl ?? globalThis.fetch.bind(globalThis)
  }

  private headers(extra: Record<string, string> = {}): Record<string, string> {
    return {
      ...(this.token ? { Authorization: `Bearer ${this.token}` } : {}),
      ...extra,
    }
  }

  private async request<T>(path: string, init: RequestInit = {}): Promise<T> {
    const response = await this.fetchImpl(`${this.baseUrl}${path}`, {
      ...init,
      headers: this.headers(init.headers as Record<string, string> | undefined),
    })
    if (!response.ok) {
      let kind = `http_${response.status}`
      let message = `${response.status} ${response.statusText}`
      const text = await response.text().catch(() => '')
      if (text) {
        try {
          const body = JSON.parse(text) as { error?: string; message?: string }
          kind = body.error ?? kind
          message = body.message ?? text
        } catch {
          message = text
        }
      }
      throw new ApiError(response.status, kind, message)
    }
    if (response.status === 204) return undefined as T
    const text = await response.text()
    return (text ? JSON.parse(text) : undefined) as T
  }

  // ------------------------------------------------------------------ accounts

  register(username: string, password: string): Promise<AuthToken> {
    return this.request<AuthToken>('/api/auth/register', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ username, password }),
    })
  }

  login(username: string, password: string): Promise<AuthToken> {
    return this.request<AuthToken>('/api/auth/login', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ username, password }),
    })
  }

  logout(): Promise<void> {
    return this.request<void>('/api/auth/logout', { method: 'POST' })
  }

  me(): Promise<PublicUser> {
    return this.request<PublicUser>('/api/me')
  }

  status(): Promise<Record<string, unknown>> {
    return this.request<Record<string, unknown>>('/api/status')
  }

  // ------------------------------------------------------------------ namespace

  /** `depth` is `0` (the node), `1` (children) or `infinity` (whole subtree); listings are flat. */
  listFiles(path = '/', depth: '0' | '1' | 'infinity' = '1'): Promise<FileNode[]> {
    const suffix = path === '/' || path === '' ? '' : encodePath(path)
    return this.request<FileNode[]>(`/api/files${suffix}?depth=${depth}`)
  }

  /** A single node (metadata only). */
  getFile(path: string): Promise<FileNode> {
    return this.request<FileNode>(`/api/files${encodePath(path)}`)
  }

  /** Create or replace a file through the JSON API (same YDoc transaction path as WebDAV). */
  putFile(path: string, content: string, visibility?: 'private' | 'public'): Promise<FileNode> {
    const query = visibility ? `?visibility=${visibility}` : ''
    return this.request<FileNode>(`/api/files${encodePath(path)}${query}`, {
      method: 'PUT',
      headers: { 'content-type': 'text/plain' },
      body: content,
    })
  }

  deleteFile(path: string): Promise<void> {
    return this.request<void>(`/api/files${encodePath(path)}`, { method: 'DELETE' })
  }

  // ------------------------------------------------------------------ WebDAV

  /** Raw WebDAV GET: the bytes the server materialises from `Y.Text("content")`. */
  async davGet(path: string): Promise<{ text: string; etag: string | null }> {
    const response = await this.fetchImpl(`${this.baseUrl}/dav${encodePath(path)}`, {
      headers: this.headers(),
    })
    if (!response.ok) throw new ApiError(response.status, 'webdav_get', await response.text())
    return { text: await response.text(), etag: response.headers.get('etag') }
  }

  davPut(path: string, content: string): Promise<Response> {
    return this.fetchImpl(`${this.baseUrl}/dav${encodePath(path)}`, {
      method: 'PUT',
      headers: this.headers({ 'content-type': 'text/plain' }),
      body: content,
    })
  }

  davMkcol(path: string): Promise<Response> {
    // `MKCOL` is not one of the methods forbidden to `fetch`, so collections can be created from
    // the browser. The JSON API only creates files.
    return this.fetchImpl(`${this.baseUrl}/dav${encodePath(path)}`, {
      method: 'MKCOL',
      headers: this.headers(),
    })
  }

  /**
   * Create every missing collection of a path with WebDAV `MKCOL`.
   *
   * `PUT /api/files/<path>` resolves an *existing* parent (`FileService::create`), while WebDAV
   * `PUT` creates the missing parents itself. Creating collections up front makes the JSON API
   * usable for nested paths.
   */
  async ensureCollections(path: string): Promise<void> {
    const segments = path.replace(/^\/+/, '').split('/').filter(Boolean)
    segments.pop() // the file name is not a collection
    let current = ''
    for (const segment of segments) {
      current += `/${segment}`
      const response = await this.davMkcol(current)
      // 405 = already there, which is the normal case.
      if (!response.ok && response.status !== 405 && response.status !== 409) {
        throw new ApiError(response.status, 'mkcol', `MKCOL ${current} -> ${await response.text()}`)
      }
    }
  }

  /** WebDAV MOVE; the node answers with a `custom(42, path)` frame to open YSync clients (§29). */
  davMove(from: string, to: string, baseUrl = ''): Promise<Response> {
    const destination = `${baseUrl}${encodePath(to)}`
    return this.fetchImpl(`${this.baseUrl}/dav${encodePath(from)}`, {
      method: 'MOVE',
      headers: this.headers({ Destination: destination, Overwrite: 'T' }),
    })
  }

  /** Metadata + the materialised YDoc content, straight from `FileService` (§3.1). */
  document(path: string): Promise<Record<string, unknown>> {
    return this.request<Record<string, unknown>>(`/api/document${encodePath(path)}`)
  }
}

/** `/private/alice/notes/todo.md` -> `/private%2Falice/notes/todo.md`-ish (segments stay readable). */
export function encodePath(path: string): string {
  const clean = path.replace(/^\/+/, '')
  if (!clean) return ''
  return `/${clean.split('/').map(encodeURIComponent).join('/')}`
}