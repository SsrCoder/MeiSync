import { describe, expect, it } from 'vitest'
import { plan, type PlanAction } from '../src/sync/planner'
import type { BaselineEntry, LocalEntry, RemoteEntry } from '../src/types'

const baseFile = (overrides: Partial<BaselineEntry> = {}): BaselineEntry => ({
  path: 'a.md',
  kind: 'file',
  etag: '"v1"',
  size: 10,
  mtime: 1000,
  version: 1,
  content_type: 'text/markdown',
  ...overrides,
})

const localFile = (path = 'a.md', mtime = 1000, size = 10): LocalEntry => ({
  path,
  kind: 'file',
  size,
  mtime,
})

const remoteFile = (path = 'a.md', etag = '"v1"', version = 1, size = 10): RemoteEntry => ({
  path,
  kind: 'file',
  etag,
  version,
  size,
  content_type: 'text/markdown',
  updated_at: '2025-01-01T00:00:00Z',
})

function run(
  baseline: Record<string, BaselineEntry>,
  local: LocalEntry[],
  remote: RemoteEntry[],
  options: { conflictStrategy?: 'duplicate' | 'local-wins' | 'remote-wins' | 'manual'; initialSync?: 'merge' | 'prefer-local' | 'prefer-remote' } = {},
): PlanAction[] {
  return plan({
    baseline,
    local: new Map(local.map((entry) => [entry.path, entry])),
    remote: new Map(remote.map((entry) => [entry.path, entry])),
    conflictStrategy: options.conflictStrategy || 'duplicate',
    initialSync: options.initialSync || 'merge',
  }).actions
}

describe('planner', () => {
  it('uploads a new local file', () => {
    const actions = run({}, [localFile('new.md')], [])
    expect(actions).toEqual([
      {
        type: 'upload',
        path: 'new.md',
        local: localFile('new.md'),
        baseEtag: null,
        force: false,
        reason: 'new local file',
      },
    ])
  })

  it('downloads a new remote file', () => {
    const actions = run({}, [], [remoteFile('new.md')])
    expect(actions[0]).toMatchObject({ type: 'download', path: 'new.md' })
  })

  it('uploads a locally modified file with its base etag', () => {
    const actions = run({ 'a.md': baseFile() }, [localFile('a.md', 2000, 11)], [remoteFile()])
    expect(actions[0]).toMatchObject({
      type: 'upload',
      baseEtag: '"v1"',
      force: false,
    })
  })

  it('downloads a remotely modified file', () => {
    const actions = run({ 'a.md': baseFile() }, [localFile()], [remoteFile('a.md', '"v2"', 2)])
    expect(actions[0]).toMatchObject({ type: 'download', reason: 'remote modified' })
  })

  it('does nothing for identical state', () => {
    expect(run({ 'a.md': baseFile() }, [localFile()], [remoteFile()])).toEqual([])
  })

  it('reports a conflict when both sides changed', () => {
    const actions = run(
      { 'a.md': baseFile() },
      [localFile('a.md', 3000, 12)],
      [remoteFile('a.md', '"v2"', 2, 11)],
    )
    expect(actions[0]).toMatchObject({ type: 'conflict', conflict: 'both-changed' })
  })

  it('resolves both-changed according to the strategy', () => {
    const local = [localFile('a.md', 3000, 12)]
    const remote = [remoteFile('a.md', '"v2"', 2, 11)]
    expect(run({ 'a.md': baseFile() }, local, remote, { conflictStrategy: 'remote-wins' })[0]).toMatchObject({
      type: 'download',
    })
    expect(run({ 'a.md': baseFile() }, local, remote, { conflictStrategy: 'local-wins' })[0]).toMatchObject({
      type: 'upload',
      force: true,
    })
    expect(run({ 'a.md': baseFile() }, local, remote, { conflictStrategy: 'manual' })[0]).toMatchObject({
      type: 'conflict',
    })
  })

  it('propagates a local deletion when the remote file is unchanged', () => {
    const actions = run({ 'a.md': baseFile() }, [], [remoteFile()])
    expect(actions[0]).toMatchObject({ type: 'delete-remote', etag: '"v1"', force: false })
  })

  it('deletes a local file when the remote file is unchanged and missing', () => {
    const actions = run({ 'a.md': baseFile() }, [localFile()], [])
    expect(actions[0]).toMatchObject({ type: 'delete-local', kind: 'file' })
  })

  it('treats local-change + remote-delete as a conflict', () => {
    const actions = run({ 'a.md': baseFile() }, [localFile('a.md', 4000, 12)], [])
    expect(actions[0]).toMatchObject({ type: 'conflict', conflict: 'local-changed-remote-deleted' })
  })

  it('treats local-delete + remote-change as a conflict', () => {
    const actions = run({ 'a.md': baseFile() }, [], [remoteFile('a.md', '"v2"', 2, 12)])
    expect(actions[0]).toMatchObject({ type: 'conflict', conflict: 'local-deleted-remote-changed' })
  })

  it('honours the initial sync mode for files created on both sides', () => {
    const local = [localFile('a.md')]
    const remote = [remoteFile('a.md', '"remote"', 1, 20)]
    expect(run({}, local, remote, { initialSync: 'merge' })[0]).toMatchObject({ type: 'conflict' })
    expect(run({}, local, remote, { initialSync: 'prefer-local' })[0]).toMatchObject({
      type: 'upload',
      force: true,
    })
    expect(run({}, local, remote, { initialSync: 'prefer-remote' })[0]).toMatchObject({ type: 'download' })
  })

  it('creates parent folders before children and deletes children before parents', () => {
    const actions = run(
      {},
      [
        { path: 'dir', kind: 'collection', size: 0, mtime: 0 },
        localFile('dir/a.md'),
      ],
      [
        { path: 'other', kind: 'collection', size: 0, version: 0, content_type: '', updated_at: '' },
        remoteFile('other/b.md'),
      ],
    )
    expect(actions.map((action) => `${action.type}:${'path' in action ? action.path : ''}`)).toEqual([
      'mkdir-remote:dir',
      'mkdir-local:other',
      'upload:dir/a.md',
      'download:other/b.md',
    ])
  })

  it('always reports a file/folder collision instead of forcing a strategy', () => {
    const local = [{ path: 'a', kind: 'file' as const, size: 3, mtime: 1 }]
    const remote = [
      { path: 'a', kind: 'collection' as const, size: 0, version: 0, content_type: '', updated_at: '' },
      { ...remoteFile('a/b.md') },
    ]
    for (const strategy of ['duplicate', 'local-wins', 'remote-wins', 'manual'] as const) {
      const actions = run({}, local, remote, { conflictStrategy: strategy })
      expect(actions[0]).toMatchObject({ type: 'conflict', path: 'a', conflict: 'kind-mismatch' })
    }
  })

  it('forgets baseline entries that disappeared from both sides', () => {
    const actions = run({ 'gone.md': baseFile({ path: 'gone.md' }) }, [], [])
    expect(actions[0]).toEqual({ type: 'forget-baseline', path: 'gone.md' })
  })
})
