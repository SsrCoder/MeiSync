import { describe, expect, it } from 'vitest'
import {
  conflictPath,
  contentTypeFor,
  globToRegExp,
  isExcluded,
  normalizeVaultPath,
  normalizeBaseUrl,
  redactToken,
  remoteVaultPath,
  websocketUrl,
} from '../src/sync/paths'
import { DEFAULT_SETTINGS } from '../src/types'

describe('paths', () => {
  it('normalises vault paths', () => {
    expect(normalizeVaultPath('/notes/a.md')).toBe('notes/a.md')
    expect(normalizeVaultPath('./notes//a.md')).toBe('notes/a.md')
    expect(() => normalizeVaultPath('notes/../secret.md')).toThrow()
  })

  it('builds the remote root from settings and vault name', () => {
    expect(remoteVaultPath({ ...DEFAULT_SETTINGS, remoteRoot: 'obsidian' }, 'My Vault')).toBe(
      'obsidian/My Vault',
    )
    expect(remoteVaultPath({ ...DEFAULT_SETTINGS, remoteRoot: '' }, 'My Vault')).toBe('My Vault')
  })

  it('matches globs with * and **', () => {
    expect(globToRegExp('*.md').test('a.md')).toBe(true)
    expect(globToRegExp('*.md').test('notes/a.md')).toBe(false)
    expect(globToRegExp('**/*.md').test('notes/a.md')).toBe(true)
    expect(globToRegExp('**/.DS_Store').test('.DS_Store')).toBe(true)
    expect(globToRegExp('assets/**').test('assets/img/a.png')).toBe(true)
  })

  it('excludes config unless enabled and always excludes plugin state', () => {
    const settings = { ...DEFAULT_SETTINGS, syncConfig: false }
    expect(isExcluded('.obsidian', settings)).toBe(true)
    expect(isExcluded('.obsidian/app.json', settings)).toBe(true)
    const withConfig = { ...DEFAULT_SETTINGS, syncConfig: true }
    expect(isExcluded('.obsidian/app.json', withConfig)).toBe(false)
    expect(isExcluded('.obsidian/plugins/meisync-obsidian/data.json', withConfig)).toBe(true)
    expect(isExcluded('.obsidian/workspace.json', withConfig)).toBe(true)
  })

  it('can skip attachments', () => {
    const settings = { ...DEFAULT_SETTINGS, syncAttachments: false }
    expect(isExcluded('assets/photo.png', settings)).toBe(true)
    expect(isExcluded('notes/a.md', settings)).toBe(false)
  })

  it('creates a deterministic conflict path', () => {
    const date = new Date(2025, 0, 2, 3, 4, 5)
    expect(conflictPath('notes/a.md', 'Mac Book', date)).toBe(
      'notes/a.conflict-Mac-Book-20250102-030405.md',
    )
  })

  it('derives content types and websocket urls', () => {
    expect(contentTypeFor('a.md')).toBe('text/markdown')
    expect(contentTypeFor('a.png')).toBe('image/png')
    expect(normalizeBaseUrl('127.0.0.1:8080/')).toBe('http://127.0.0.1:8080')
    expect(websocketUrl('http://127.0.0.1:8080', 'obsidian/MyVault', 'tok')).toContain(
      'ws://127.0.0.1:8080/api/sync/ws?',
    )
  })

  it('never leaves the session token in a loggable url', () => {
    const token = 'eyJhbGciOiJIUzI1NiJ9.bearer-secret'
    const display = redactToken(websocketUrl('http://127.0.0.1:8080', 'obsidian/MyVault', token))
    expect(display).not.toContain(token)
    expect(display).toContain('token=…')
    expect(display).toContain('vault=obsidian%2FMyVault')

    // Without a token the URL is untouched; other query parameters survive redaction.
    const plain = 'ws://127.0.0.1:8080/ysync/notes/a.md?vault=v'
    expect(redactToken(plain)).toBe(plain)
    const extra = redactToken(`${plain}&token=${token}&x=1`)
    expect(extra).not.toContain(token)
    expect(extra).toContain('x=1')
  })
})
