import type {
  BaselineEntry,
  ConflictStrategy,
  InitialSyncMode,
  LocalEntry,
  RemoteEntry,
} from '../types'

export type ConflictKind =
  | 'both-created'
  | 'both-changed'
  | 'local-changed-remote-deleted'
  | 'local-deleted-remote-changed'
  | 'kind-mismatch'

export type PlanAction =
  | { type: 'download'; path: string; remote: RemoteEntry; reason: string }
  | {
      type: 'upload'
      path: string
      local: LocalEntry
      baseEtag: string | null
      force: boolean
      reason: string
    }
  | { type: 'delete-local'; path: string; kind: 'file' | 'collection'; reason: string }
  | {
      type: 'delete-remote'
      path: string
      kind: 'file' | 'collection'
      etag?: string
      force: boolean
      reason: string
    }
  | { type: 'mkdir-local'; path: string }
  | { type: 'mkdir-remote'; path: string }
  | {
      type: 'conflict'
      path: string
      conflict: ConflictKind
      local?: LocalEntry
      remote?: RemoteEntry
      base?: BaselineEntry
    }
  | { type: 'forget-baseline'; path: string }

export interface PlanInput {
  baseline: Record<string, BaselineEntry>
  local: Map<string, LocalEntry>
  remote: Map<string, RemoteEntry>
  conflictStrategy: ConflictStrategy
  initialSync: InitialSyncMode
}

export interface PlanResult {
  actions: PlanAction[]
  /** Paths whose local and remote state disagree but which need no action beyond a baseline update. */
  unchanged: string[]
}

/**
 * Decide what to do for every path present in the local tree, the remote manifest or the last
 * synced baseline.
 *
 * The planner is intentionally pure: it performs no IO. That makes the conflict rules testable
 * and keeps the engine responsible for ordering, retries and baseline bookkeeping.
 */
export function plan(input: PlanInput): PlanResult {
  const { baseline, local, remote } = input
  const paths = new Set<string>([
    ...Object.keys(baseline),
    ...local.keys(),
    ...remote.keys(),
  ])
  const actions: PlanAction[] = []
  const unchanged: string[] = []

  for (const path of [...paths].sort()) {
    const base = baseline[path]
    const localEntry = local.get(path)
    const remoteEntry = remote.get(path)

    if (!base) {
      planWithoutBaseline(path, localEntry, remoteEntry, input, actions, unchanged)
      continue
    }

    if (!localEntry && !remoteEntry) {
      actions.push({ type: 'forget-baseline', path })
      continue
    }

    // Directories are structural. Create them on the missing side, but never delete a directory
    // from a single missing manifest row: its children decide, and the engine only removes empty
    // folders at the very end.
    if (base.kind === 'collection' || localEntry?.kind === 'collection' || remoteEntry?.kind === 'collection') {
      planDirectory(path, base, localEntry, remoteEntry, actions, unchanged)
      continue
    }

    const localChanged = hasLocalChanged(base, localEntry)
    const remoteChanged = hasRemoteChanged(base, remoteEntry)

    if (localEntry && remoteEntry) {
      if (!localChanged && !remoteChanged) {
        unchanged.push(path)
      } else if (localChanged && !remoteChanged) {
        actions.push({
          type: 'upload',
          path,
          local: localEntry,
          baseEtag: base.etag ?? null,
          force: false,
          reason: 'local modified',
        })
      } else if (!localChanged && remoteChanged) {
        actions.push({ type: 'download', path, remote: remoteEntry, reason: 'remote modified' })
      } else {
        pushConflict(path, 'both-changed', localEntry, remoteEntry, base, input.conflictStrategy, actions)
      }
      continue
    }

    if (localEntry && !remoteEntry) {
      // The file existed in the baseline, so the remote side deleted it.
      if (localChanged) {
        pushConflict(
          path,
          'local-changed-remote-deleted',
          localEntry,
          undefined,
          base,
          input.conflictStrategy,
          actions,
        )
      } else {
        actions.push({ type: 'delete-local', path, kind: 'file', reason: 'remote deleted' })
      }
      continue
    }

    if (!localEntry && remoteEntry) {
      // The file existed in the baseline, so the local side deleted it.
      if (remoteChanged) {
        pushConflict(
          path,
          'local-deleted-remote-changed',
          undefined,
          remoteEntry,
          base,
          input.conflictStrategy,
          actions,
        )
      } else {
        actions.push({
          type: 'delete-remote',
          path,
          kind: base.kind,
          etag: base.etag,
          force: false,
          reason: 'local deleted',
        })
      }
    }
  }

  actions.sort(compareActions)
  return { actions, unchanged }
}

function planWithoutBaseline(
  path: string,
  localEntry: LocalEntry | undefined,
  remoteEntry: RemoteEntry | undefined,
  input: PlanInput,
  actions: PlanAction[],
  unchanged: string[],
): void {
  if (!localEntry && remoteEntry) {
    if (remoteEntry.kind === 'collection') actions.push({ type: 'mkdir-local', path })
    else actions.push({ type: 'download', path, remote: remoteEntry, reason: 'new remote file' })
    return
  }
  if (localEntry && !remoteEntry) {
    if (localEntry.kind === 'collection') actions.push({ type: 'mkdir-remote', path })
    else
      actions.push({
        type: 'upload',
        path,
        local: localEntry,
        baseEtag: null,
        force: false,
        reason: 'new local file',
      })
    return
  }
  if (!localEntry || !remoteEntry) return

  if (localEntry.kind === 'collection' && remoteEntry.kind === 'collection') {
    unchanged.push(path)
    return
  }
  if (localEntry.kind !== remoteEntry.kind) {
    pushConflict(path, 'kind-mismatch', localEntry, remoteEntry, undefined, input.conflictStrategy, actions)
    return
  }
  if (input.initialSync === 'prefer-remote') {
    actions.push({ type: 'download', path, remote: remoteEntry, reason: 'initial sync: remote wins' })
    return
  }
  if (input.initialSync === 'prefer-local') {
    actions.push({
      type: 'upload',
      path,
      local: localEntry,
      baseEtag: null,
      force: true,
      reason: 'initial sync: local wins',
    })
    return
  }
  // Merge: both sides created a file at the same path independently.
  pushConflict(path, 'both-created', localEntry, remoteEntry, undefined, input.conflictStrategy, actions)
}

function planDirectory(
  path: string,
  base: BaselineEntry,
  localEntry: LocalEntry | undefined,
  remoteEntry: RemoteEntry | undefined,
  actions: PlanAction[],
  unchanged: string[],
): void {
  const localKind = localEntry?.kind
  const remoteKind = remoteEntry?.kind

  if (localKind === 'file' || remoteKind === 'file') {
    // A file/directory collision cannot be merged automatically; the engine reports it and keeps
    // the local side untouched.
    actions.push({
      type: 'conflict',
      path,
      conflict: 'kind-mismatch',
      local: localEntry,
      remote: remoteEntry,
      base,
    })
    return
  }
  if (!localEntry && remoteEntry) {
    actions.push({ type: 'mkdir-local', path })
    return
  }
  if (localEntry && !remoteEntry) {
    if (base) actions.push({ type: 'delete-local', path, kind: 'collection', reason: 'remote removed folder' })
    else actions.push({ type: 'mkdir-remote', path })
    return
  }
  if (localEntry && remoteEntry) {
    unchanged.push(path)
  }
}

function hasLocalChanged(base: BaselineEntry, localEntry: LocalEntry | undefined): boolean {
  if (!localEntry) return true
  if (localEntry.kind !== base.kind) return true
  if (localEntry.kind === 'collection') return false
  // File systems have millisecond timestamps; a one millisecond tolerance avoids ping-pong from
  // rounding when a file is written by another application.
  return localEntry.size !== base.size || Math.abs(localEntry.mtime - base.mtime) > 1
}

function hasRemoteChanged(base: BaselineEntry, remoteEntry: RemoteEntry | undefined): boolean {
  if (!remoteEntry) return true
  if (remoteEntry.kind !== base.kind) return true
  if (remoteEntry.kind === 'collection') return false
  if (remoteEntry.etag !== base.etag) return true
  return remoteEntry.version !== base.version
}

function pushConflict(
  path: string,
  conflict: ConflictKind,
  localEntry: LocalEntry | undefined,
  remoteEntry: RemoteEntry | undefined,
  base: BaselineEntry | undefined,
  strategy: ConflictStrategy,
  actions: PlanAction[],
): void {
  // A file/folder collision has no safe automatic resolution: uploading would target a
  // collection and downloading would target a file. Always report it for a human.
  if (conflict === 'kind-mismatch') {
    actions.push({ type: 'conflict', path, conflict, local: localEntry, remote: remoteEntry, base })
    return
  }
  switch (strategy) {
    case 'remote-wins':
      if (remoteEntry) {
        if (remoteEntry.kind === 'collection') actions.push({ type: 'mkdir-local', path })
        else actions.push({ type: 'download', path, remote: remoteEntry, reason: `conflict: ${conflict}` })
      } else if (localEntry) {
        actions.push({ type: 'delete-local', path, kind: localEntry.kind, reason: `conflict: ${conflict}` })
      } else {
        actions.push({ type: 'forget-baseline', path })
      }
      return
    case 'local-wins':
      if (localEntry) {
        if (localEntry.kind === 'collection') actions.push({ type: 'mkdir-remote', path })
        else
          actions.push({
            type: 'upload',
            path,
            local: localEntry,
            baseEtag: base?.etag ?? null,
            force: true,
            reason: `conflict: ${conflict}`,
          })
      } else if (remoteEntry) {
        actions.push({
          type: 'delete-remote',
          path,
          kind: remoteEntry.kind,
          etag: remoteEntry.etag,
          force: true,
          reason: `conflict: ${conflict}`,
        })
      } else {
        actions.push({ type: 'forget-baseline', path })
      }
      return
    default:
      actions.push({ type: 'conflict', path, conflict, local: localEntry, remote: remoteEntry, base })
  }
}

function priority(action: PlanAction): number {
  switch (action.type) {
    case 'mkdir-local':
    case 'mkdir-remote':
      return 0
    // A folder/file collision must be reported before children are downloaded into the path
    // that the local file currently occupies.
    case 'conflict':
      return 1
    case 'download':
    case 'upload':
      return 2
    case 'delete-local':
    case 'delete-remote':
      return 3
    default:
      return 4
  }
}

function compareActions(a: PlanAction, b: PlanAction): number {
  const byPriority = priority(a) - priority(b)
  if (byPriority !== 0) return byPriority
  const aPath = 'path' in a ? a.path : ''
  const bPath = 'path' in b ? b.path : ''
  if (a.type === 'delete-local' || a.type === 'delete-remote') {
    // Children before parents: deleting a folder must happen after its files.
    return bPath.split('/').length - aPath.split('/').length || aPath.localeCompare(bPath)
  }
  if (a.type === 'mkdir-local' || a.type === 'mkdir-remote') {
    // Parents before children.
    return aPath.split('/').length - bPath.split('/').length || aPath.localeCompare(bPath)
  }
  return aPath.localeCompare(bPath)
}

/** Convenience used by the engine after a sync round. */
export function baselineFrom(
  local: Map<string, LocalEntry>,
  remote: Map<string, RemoteEntry>,
): Record<string, BaselineEntry> {
  const baseline: Record<string, BaselineEntry> = Object.create(null) as Record<string, BaselineEntry>
  for (const [path, localEntry] of local) {
    const remoteEntry = remote.get(path)
    if (!remoteEntry || remoteEntry.kind !== localEntry.kind) continue
    if (remoteEntry.kind === 'collection') {
      baseline[path] = {
        path,
        kind: 'collection',
        size: 0,
        mtime: 0,
        version: remoteEntry.version,
        content_type: '',
      }
      continue
    }
    baseline[path] = {
      path,
      kind: 'file',
      etag: remoteEntry.etag,
      size: localEntry.size,
      mtime: localEntry.mtime,
      version: remoteEntry.version,
      content_type: remoteEntry.content_type,
    }
  }
  return baseline
}
