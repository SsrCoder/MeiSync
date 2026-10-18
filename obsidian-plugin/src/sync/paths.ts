import type { MeiSyncSettings } from '../types'

const DEFAULT_EXCLUDES = [
  '.git',
  '.git/**',
  '.trash',
  '.trash/**',
  '.DS_Store',
  '**/.DS_Store',
  'Thumbs.db',
  '**/Thumbs.db',
  '*.tmp',
  '**/~$*',
  '**/.~lock.*',
]

/** The plugin's own state contains the token and must never be uploaded. */
const ALWAYS_EXCLUDES = [
  '.obsidian/plugins/meisync-obsidian',
  '.obsidian/plugins/meisync-obsidian/**',
  '.obsidian/plugins/meisync-obsidian/data.json',
  '.obsidian/workspace.json',
  '.obsidian/workspace-mobile.json',
  '.obsidian/cache',
  '.obsidian/**/*.log',
]

const TEXT_EXTENSIONS = new Set([
  'md',
  'markdown',
  'mdx',
  'canvas',
  'base',
  'txt',
  'json',
  'jsonc',
  'yaml',
  'yml',
  'toml',
  'csv',
  'tsv',
  'html',
  'htm',
  'css',
  'scss',
  'less',
  'js',
  'mjs',
  'cjs',
  'jsx',
  'ts',
  'tsx',
  'vue',
  'svelte',
  'py',
  'rs',
  'go',
  'java',
  'kt',
  'c',
  'h',
  'cpp',
  'hpp',
  'cs',
  'rb',
  'php',
  'sh',
  'bash',
  'zsh',
  'fish',
  'ps1',
  'sql',
  'xml',
  'svg',
  'tex',
  'org',
  'rst',
  'log',
  'env',
  'gitignore',
  'properties',
])

const MIME: Record<string, string> = {
  md: 'text/markdown',
  markdown: 'text/markdown',
  mdx: 'text/markdown',
  canvas: 'application/json',
  base: 'application/json',
  txt: 'text/plain',
  json: 'application/json',
  jsonc: 'application/json',
  yaml: 'application/yaml',
  yml: 'application/yaml',
  toml: 'application/toml',
  csv: 'text/csv',
  tsv: 'text/tab-separated-values',
  html: 'text/html',
  htm: 'text/html',
  css: 'text/css',
  js: 'text/javascript',
  mjs: 'text/javascript',
  jsx: 'text/javascript',
  ts: 'text/typescript',
  tsx: 'text/typescript',
  py: 'text/x-python',
  rs: 'text/x-rust',
  go: 'text/x-go',
  sh: 'text/x-sh',
  xml: 'application/xml',
  svg: 'image/svg+xml',
  png: 'image/png',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  gif: 'image/gif',
  webp: 'image/webp',
  avif: 'image/avif',
  bmp: 'image/bmp',
  ico: 'image/x-icon',
  pdf: 'application/pdf',
  mp3: 'audio/mpeg',
  wav: 'audio/wav',
  m4a: 'audio/mp4',
  ogg: 'audio/ogg',
  flac: 'audio/flac',
  mp4: 'video/mp4',
  webm: 'video/webm',
  mov: 'video/quicktime',
  zip: 'application/zip',
  gz: 'application/gzip',
  epub: 'application/epub+zip',
}

/** Normalise an Obsidian vault path: no leading slash, no `.`/`..`, `/` separators. */
export function normalizeVaultPath(input: string): string {
  const raw = String(input || '')
    .replace(/\\/g, '/')
    .replace(/^\.\/+/, '')
    .trim()
  const parts: string[] = []
  for (const part of raw.split('/')) {
    const segment = part.trim()
    if (!segment || segment === '.') continue
    if (segment === '..') throw new Error(`invalid path: ${input}`)
    if (segment.includes('\0')) throw new Error('invalid path')
    parts.push(segment)
  }
  return parts.join('/')
}

/** A safe, human readable remote root: `obsidian/My Vault` -> `obsidian/My Vault`. */
export function remoteVaultPath(settings: MeiSyncSettings, actualVaultName: string): string {
  const root = String(settings.remoteRoot || '')
    .split('/')
    .map((part) => part.trim())
    .filter((part) => part && part !== '.' && part !== '..')
    .join('/')
  const name = sanitizeSegment(settings.vaultNameOverride.trim() || actualVaultName || 'vault')
  return root ? `${root}/${name}` : name
}

export function sanitizeSegment(value: string): string {
  return String(value || '')
    .replace(/[\\/:*?"<>|]/g, '-')
    .replace(/\s+/g, ' ')
    .trim()
    .slice(0, 120)
}

export function parentPath(path: string): string {
  const index = path.lastIndexOf('/')
  return index === -1 ? '' : path.slice(0, index)
}

export function baseName(path: string): string {
  const index = path.lastIndexOf('/')
  return index === -1 ? path : path.slice(index + 1)
}

export function extension(path: string): string {
  const name = baseName(path)
  const index = name.lastIndexOf('.')
  if (index <= 0) return ''
  return name.slice(index + 1).toLowerCase()
}

export function isTextPath(path: string): boolean {
  return TEXT_EXTENSIONS.has(extension(path))
}

export function isAttachmentPath(path: string): boolean {
  return !isTextPath(path)
}

export function contentTypeFor(path: string): string {
  return MIME[extension(path)] || 'application/octet-stream'
}

/** Convert a glob to a regular expression. Supports `*`, `**` and `?`. */
export function globToRegExp(glob: string): RegExp {
  let pattern = ''
  const value = glob.replace(/\\/g, '/').trim()
  for (let index = 0; index < value.length; index += 1) {
    const char = value[index]
    if (char === '*') {
      if (value[index + 1] === '*') {
        index += 1
        if (value[index + 1] === '/') {
          index += 1
          pattern += '(?:.*/)?'
        } else {
          pattern += '.*'
        }
      } else {
        pattern += '[^/]*'
      }
      continue
    }
    if (char === '?') {
      pattern += '[^/]'
      continue
    }
    pattern += char.replace(/[.+^${}()|[\]\\]/g, '\\$&')
  }
  return new RegExp(`^${pattern}$`, 'i')
}

export function exclusionPatterns(settings: MeiSyncSettings): RegExp[] {
  const patterns = [...DEFAULT_EXCLUDES, ...ALWAYS_EXCLUDES]
  if (!settings.syncConfig) patterns.push('.obsidian', '.obsidian/**')
  for (const line of String(settings.excludePatterns || '').split(/\r?\n/)) {
    const value = line.trim()
    if (!value || value.startsWith('#')) continue
    patterns.push(value)
  }
  return patterns.map(globToRegExp)
}

export function isExcluded(path: string, settings: MeiSyncSettings, patterns?: RegExp[]): boolean {
  const normalized = path.replace(/\\/g, '/').replace(/^\/+/, '')
  if (!normalized) return true
  if (!settings.syncAttachments && isAttachmentPath(normalized)) return true
  const matchers = patterns || exclusionPatterns(settings)
  return matchers.some((matcher) => matcher.test(normalized))
}

/** `notes/a.md` + `meisync-macbook` + timestamp -> `notes/a.conflict-meisync-macbook-20250101-120000.md`. */
export function conflictPath(path: string, deviceLabel: string, date = new Date()): string {
  const parent = parentPath(path)
  const name = baseName(path)
  const extIndex = name.lastIndexOf('.')
  const stem = extIndex > 0 ? name.slice(0, extIndex) : name
  const ext = extIndex > 0 ? name.slice(extIndex) : ''
  const stamp = [
    date.getFullYear(),
    String(date.getMonth() + 1).padStart(2, '0'),
    String(date.getDate()).padStart(2, '0'),
    '-',
    String(date.getHours()).padStart(2, '0'),
    String(date.getMinutes()).padStart(2, '0'),
    String(date.getSeconds()).padStart(2, '0'),
  ].join('')
  const label = sanitizeSegment(deviceLabel).replace(/\s+/g, '-') || 'device'
  const conflict = `${stem}.conflict-${label}-${stamp}${ext}`
  return parent ? `${parent}/${conflict}` : conflict
}

/** Build the WebSocket URL of the change feed from the HTTP base URL. */
export function websocketUrl(baseUrl: string, vault: string, token: string): string {
  const base = normalizeBaseUrl(baseUrl).replace(/^http:/, 'ws:').replace(/^https:/, 'wss:')
  const params = new URLSearchParams({ vault, token })
  return `${base}/api/sync/ws?${params.toString()}`
}

/**
 * A loggable form of a WebSocket URL: the session token in the query string is replaced with `…`.
 *
 * Browsers cannot set headers on a WebSocket upgrade, so the token has to travel as `?token=`. The
 * authenticated URL must therefore never be interpolated into a message: socket errors (server
 * down, TLS failure, rejected upgrade) end up in the sync log, the devtools console and the
 * "Copy" clipboard buffer, and would carry a live bearer token with them.
 */
export function redactToken(url: string): string {
  try {
    const parsed = new URL(url)
    if (!parsed.searchParams.has('token')) return url
    parsed.searchParams.set('token', '…')
    // `URLSearchParams` percent-encodes the marker; keep the log readable.
    return parsed.toString().replace(encodeURIComponent('…'), '…')
  } catch {
    return url.replace(/([?&]token=)[^&#]*/gi, '$1…')
  }
}

export function normalizeBaseUrl(value: string): string {
  let base = String(value || '').trim().replace(/\/+$/, '')
  if (!base) throw new Error('server URL is empty')
  if (!/^https?:\/\//i.test(base)) base = `http://${base}`
  return base
}
