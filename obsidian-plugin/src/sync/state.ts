import type { BaselineEntry, PersistedState } from '../types'
import { baselineFrom } from './planner'
import type { LocalEntry, RemoteEntry } from '../types'

export function createState(raw?: Partial<PersistedState> | null): PersistedState {
  return {
    deviceId: raw?.deviceId || generateDeviceId(),
    baseline:
      raw?.baseline && typeof raw.baseline === 'object'
        ? Object.assign(Object.create(null) as Record<string, BaselineEntry>, raw.baseline)
        : (Object.create(null) as Record<string, BaselineEntry>),
    lastSyncAt: raw?.lastSyncAt,
    lastManifestRevision: raw?.lastManifestRevision,
    lastRoot: raw?.lastRoot,
  }
}

export function generateDeviceId(): string {
  const cryptoApi = globalThis.crypto
  if (cryptoApi && typeof cryptoApi.randomUUID === 'function') {
    return cryptoApi.randomUUID()
  }
  return `meisync-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 12)}`
}

export function rebuildBaseline(
  local: Map<string, LocalEntry>,
  remote: Map<string, RemoteEntry>,
  previous: Record<string, BaselineEntry>,
  skipped: Set<string>,
  expected: Map<string, LocalEntry>,
): Record<string, BaselineEntry> {
  const rebuilt = baselineFrom(local, remote)
  // A file the user touched while the round was running must not be recorded as synced: the new
  // entry would pair the ETag of a manifest fetched *before* the edit with the local stat observed
  // *after* it, so the next round would see neither side changed and never upload the edit. Keep
  // the previous entry instead, which makes that round treat the path as a local change.
  // `expected` is the stat each path should have once the round is done (the plan-time scan, plus
  // the files the round wrote itself), so a download is not mistaken for a user edit.
  for (const [path, entry] of Object.entries(rebuilt)) {
    if (entry.kind !== 'file') continue
    const before = expected.get(path)
    const now = local.get(path)
    if (!before || before.kind !== 'file' || !now) continue
    if (before.size === now.size && Math.abs(before.mtime - now.mtime) <= 1) continue
    delete rebuilt[path]
    const untouched = previous[path]
    if (untouched) rebuilt[path] = untouched
  }
  // A path that was skipped (too large, failed action, unresolved conflict) must not be marked
  // as synced. Keep the previous baseline so the next round retries it, or leave it absent when
  // there was no baseline yet (a new file that failed to upload).
  for (const path of skipped) {
    delete rebuilt[path]
    const entry = previous[path]
    if (entry) rebuilt[path] = entry
  }
  return rebuilt
}

export function baselineSize(baseline: Record<string, BaselineEntry>): number {
  return Object.keys(baseline).length
}
