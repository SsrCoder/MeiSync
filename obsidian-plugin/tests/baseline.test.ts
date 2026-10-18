import { describe, expect, it } from 'vitest'
import { rebuildBaseline } from '../src/sync/state'
import type { BaselineEntry, LocalEntry, RemoteEntry } from '../src/types'

const localFile = (path: string, size: number, mtime: number): LocalEntry => ({
  path,
  kind: 'file',
  size,
  mtime,
})

const remoteFile = (path: string, etag = '"v1"', version = 1, size = 10): RemoteEntry => ({
  path,
  kind: 'file',
  etag,
  version,
  size,
  content_type: 'text/markdown',
  updated_at: '2025-01-01T00:00:00Z',
})

const baseFile = (path: string, overrides: Partial<BaselineEntry> = {}): BaselineEntry => ({
  path,
  kind: 'file',
  etag: '"v1"',
  size: 10,
  mtime: 1000,
  version: 1,
  content_type: 'text/markdown',
  ...overrides,
})

function byPath<T extends { path: string }>(entries: T[]): Map<string, T> {
  return new Map(entries.map((entry) => [entry.path, entry]))
}

describe('rebuildBaseline', () => {
  it('does not record a file that was edited while the round was running', () => {
    const planned = byPath([localFile('a.md', 10, 1000)])
    // The user saved the note after the plan was computed but before the baseline refresh.
    const fresh = byPath([localFile('a.md', 24, 5000)])
    const remote = byPath([remoteFile('a.md')])

    const rebuilt = rebuildBaseline(fresh, remote, { 'a.md': baseFile('a.md') }, new Set(), planned)

    // The previous entry survives, so the next round sees a local change and uploads it.
    expect(rebuilt['a.md']).toEqual(baseFile('a.md'))
  })

  it('leaves a mid-round edit without a previous entry out of the baseline', () => {
    const planned = byPath([localFile('a.md', 10, 1000)])
    const fresh = byPath([localFile('a.md', 24, 5000)])
    const remote = byPath([remoteFile('a.md')])

    const rebuilt = rebuildBaseline(fresh, remote, {}, new Set(), planned)

    // Never claim the un-uploaded content is in sync; the next round resolves it as a conflict.
    expect(rebuilt['a.md']).toBeUndefined()
  })

  it('records an unchanged file normally', () => {
    const planned = byPath([localFile('a.md', 10, 1000)])
    const fresh = byPath([localFile('a.md', 10, 1000)])
    const remote = byPath([remoteFile('a.md', '"v2"', 2, 12)])

    const rebuilt = rebuildBaseline(fresh, remote, {}, new Set(), planned)

    expect(rebuilt['a.md']).toEqual({
      path: 'a.md',
      kind: 'file',
      etag: '"v2"',
      size: 10,
      mtime: 1000,
      version: 2,
      content_type: 'text/markdown',
    })
  })

  it('tolerates one millisecond of mtime jitter', () => {
    const planned = byPath([localFile('a.md', 10, 1000)])
    const fresh = byPath([localFile('a.md', 10, 1001)])
    const remote = byPath([remoteFile('a.md', '"v2"', 2)])

    const rebuilt = rebuildBaseline(fresh, remote, {}, new Set(), planned)

    expect(rebuilt['a.md']).toMatchObject({ etag: '"v2"', mtime: 1001 })
  })

  it('keeps the previous entry for a skipped path', () => {
    const planned = byPath([localFile('a.md', 10, 1000)])
    const fresh = byPath([localFile('a.md', 10, 1000)])
    const remote = byPath([remoteFile('a.md', '"v1"', 1)])
    const previous = { 'a.md': baseFile('a.md', { etag: '"v0"', version: 0 }) }

    const rebuilt = rebuildBaseline(fresh, remote, previous, new Set(['a.md']), planned)

    expect(rebuilt['a.md']).toEqual(previous['a.md'])
  })

  it('records a path that appeared after the plan, such as a conflict copy', () => {
    const planned = byPath([localFile('a.md', 10, 1000)])
    const fresh = byPath([
      localFile('a.md', 10, 1000),
      localFile('a.conflict-device-20250101-120000.md', 10, 1000),
    ])
    const remote = byPath([
      remoteFile('a.md', '"v2"', 2),
      remoteFile('a.conflict-device-20250101-120000.md', '"v3"', 1),
    ])

    const rebuilt = rebuildBaseline(fresh, remote, {}, new Set(), planned)

    expect(rebuilt['a.conflict-device-20250101-120000.md']).toMatchObject({ etag: '"v3"' })
  })
})
