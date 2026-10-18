import { requestUrl, type RequestUrlResponse } from 'obsidian'
import { contentTypeFor, normalizeBaseUrl } from './sync/paths'
import type { DeviceInfo, Handshake, RemoteManifest, WriteResult } from './types'

export class SyncHttpError extends Error {
  constructor(
    readonly status: number,
    message: string,
    readonly body?: unknown,
  ) {
    super(message)
    this.name = 'SyncHttpError'
  }
}

export class ConflictError extends SyncHttpError {
  constructor(message: string, body?: unknown) {
    super(412, message, body)
    this.name = 'ConflictError'
  }
}

export interface LoginResult {
  token: string
  expires_at: string
  user: { id: string; username: string; created_at?: string }
}

export interface DeviceListResult {
  devices: Array<{
    device_id: string
    vault: string
    name: string
    platform: string
    plugin_version: string
    last_seen_at: string
  }>
}

interface RequestOptions {
  method?: 'GET' | 'POST' | 'PUT' | 'DELETE'
  body?: string | ArrayBuffer
  headers?: Record<string, string>
  auth?: boolean
  contentType?: string
}

/**
 * Small HTTP client for the MeiSync sync API.
 *
 * `requestUrl` is used instead of `fetch` so the plugin is not affected by CORS and works the
 * same on desktop and mobile.
 */
export class SyncApi {
  private baseUrl: string
  private token: string

  constructor(serverUrl: string, token: string, private readonly pluginVersion: string) {
    this.baseUrl = normalizeBaseUrl(serverUrl)
    this.token = token
  }

  get serverUrl(): string {
    return this.baseUrl
  }

  setServerUrl(value: string): void {
    this.baseUrl = normalizeBaseUrl(value)
  }

  setToken(token: string): void {
    this.token = token
  }

  getToken(): string {
    return this.token
  }

  private headers(options: RequestOptions): Record<string, string> {
    const headers: Record<string, string> = {
      'X-MeiSync-Client': `obsidian/${this.pluginVersion}`,
      ...(options.headers || {}),
    }
    if (options.auth !== false && this.token) {
      headers.Authorization = `Bearer ${this.token}`
    }
    if (options.contentType) headers['Content-Type'] = options.contentType
    return headers
  }

  private async request(options: RequestOptions & { path: string }): Promise<RequestUrlResponse> {
    const headers = this.headers(options)
    try {
      return await requestUrl({
        url: `${this.baseUrl}${options.path}`,
        method: options.method || 'GET',
        headers,
        body: options.body,
        throw: false,
      })
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error)
      throw new SyncHttpError(0, `network error: ${message}`)
    }
  }

  private async json<T>(
    options: RequestOptions & { path: string },
  ): Promise<{ status: number; data: T }> {
    const response = await this.request({
      ...options,
      contentType: options.contentType || 'application/json',
    })
    if (response.status < 200 || response.status >= 300) {
      throw this.error(response)
    }
    const text = response.text || ''
    return {
      status: response.status,
      data: text ? (JSON.parse(text) as T) : (undefined as T),
    }
  }

  private error(response: RequestUrlResponse): SyncHttpError {
    let body: unknown
    let message = `${response.status} ${response.status === 0 ? 'network error' : 'HTTP error'}`
    try {
      body = response.json
      const parsed = body as { message?: string; error?: string }
      message = parsed?.message || parsed?.error || message
    } catch {
      if (response.text) message = response.text.slice(0, 500)
    }
    if (response.status === 412) return new ConflictError(message, body)
    return new SyncHttpError(response.status, message, body)
  }

  async login(username: string, password: string): Promise<LoginResult> {
    const { data } = await this.json<LoginResult>({
      path: '/api/auth/login',
      method: 'POST',
      auth: false,
      body: JSON.stringify({ username, password }),
    })
    this.token = data.token
    return data
  }

  async me(): Promise<{ id: string; username: string }> {
    const { data } = await this.json<{ id: string; username: string }>({
      path: '/api/me',
    })
    return data
  }

  async syncInfo(): Promise<{ service: string; version: string; protocol: number }> {
    const { data } = await this.json<{ service: string; version: string; protocol: number }>({
      path: '/api/sync/info',
      auth: false,
    })
    return data
  }

  async handshake(vault: string, device: DeviceInfo): Promise<Handshake> {
    const { data } = await this.json<Handshake>({
      path: '/api/sync/handshake',
      method: 'POST',
      body: JSON.stringify({
        vault,
        device_id: device.deviceId,
        name: device.name,
        platform: device.platform,
        plugin_version: device.pluginVersion,
      }),
    })
    return data
  }

  async manifest(
    vault: string,
    revision?: string,
  ): Promise<{ notModified: boolean; data?: RemoteManifest; revision?: string }> {
    const headers: Record<string, string> = {}
    if (revision) headers['If-None-Match'] = `"${revision.replace(/^"|"$/g, '')}"`
    const response = await this.request({
      path: `/api/sync/manifest?vault=${encodeURIComponent(vault)}`,
      headers,
    })
    if (response.status === 304) {
      return { notModified: true, revision }
    }
    if (response.status < 200 || response.status >= 300) throw this.error(response)
    const data = JSON.parse(response.text) as RemoteManifest
    return { notModified: false, data, revision: data.revision }
  }

  async getFile(
    vault: string,
    path: string,
    etag?: string,
  ): Promise<{ notModified: boolean; data?: ArrayBuffer; etag?: string }> {
    const headers: Record<string, string> = {}
    if (etag) headers['If-None-Match'] = etag
    const response = await this.request({
      path: `/api/sync/file?vault=${encodeURIComponent(vault)}&path=${encodeURIComponent(path)}`,
      headers,
    })
    if (response.status === 304) {
      return { notModified: true, etag }
    }
    if (response.status < 200 || response.status >= 300) throw this.error(response)
    // `arrayBuffer` is available in the current Obsidian API; the text fallback keeps the
    // plugin usable on older Electron builds for text files, while truly binary content needs
    // the arrayBuffer path.
    const data =
      response.arrayBuffer ||
      (response.text ? new TextEncoder().encode(response.text).buffer : new ArrayBuffer(0))
    return {
      notModified: false,
      data,
      etag: response.headers?.etag || response.headers?.ETag,
    }
  }

  async putFile(
    vault: string,
    path: string,
    data: ArrayBuffer,
    options: { baseEtag?: string | null; force?: boolean; contentType?: string } = {},
  ): Promise<WriteResult> {
    const headers: Record<string, string> = {}
    if (options.baseEtag && !options.force) headers['If-Match'] = options.baseEtag
    else if (!options.force) headers['If-None-Match'] = '*'
    const { data: result } = await this.json<WriteResult>({
      path: `/api/sync/file?vault=${encodeURIComponent(vault)}&path=${encodeURIComponent(path)}`,
      method: 'PUT',
      headers,
      body: data,
      contentType: options.contentType || contentTypeFor(path),
    })
    return result
  }

  async deleteFile(
    vault: string,
    path: string,
    options: { etag?: string; force?: boolean } = {},
  ): Promise<void> {
    const headers: Record<string, string> = {}
    if (options.etag && !options.force) headers['If-Match'] = options.etag
    const response = await this.request({
      path: `/api/sync/file?vault=${encodeURIComponent(vault)}&path=${encodeURIComponent(path)}`,
      method: 'DELETE',
      headers,
    })
    if (response.status < 200 || response.status >= 300) throw this.error(response)
  }

  async move(vault: string, from: string, to: string, overwrite = false): Promise<void> {
    await this.json({
      path: '/api/sync/move',
      method: 'POST',
      body: JSON.stringify({ vault, from, to, overwrite }),
    })
  }

  async mkdir(vault: string, path: string): Promise<void> {
    await this.json({
      path: '/api/sync/mkdir',
      method: 'POST',
      body: JSON.stringify({ vault, path }),
    })
  }

  async registerDevice(vault: string, device: DeviceInfo): Promise<void> {
    await this.json({
      path: '/api/sync/devices',
      method: 'POST',
      body: JSON.stringify({
        vault,
        device_id: device.deviceId,
        name: device.name,
        platform: device.platform,
        plugin_version: device.pluginVersion,
      }),
    })
  }

  async listDevices(): Promise<DeviceListResult> {
    const { data } = await this.json<DeviceListResult>({ path: '/api/sync/devices' })
    return data
  }

  async health(): Promise<{ status: string; service?: string }> {
    const { data } = await this.json<{ status: string; service?: string }>({
      path: '/api/health',
      auth: false,
    })
    return data
  }
}
