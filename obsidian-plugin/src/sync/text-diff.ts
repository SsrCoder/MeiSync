export interface DiffPatch {
  index: number
  deleteCount: number
  insert: string
}

/**
 * Smallest single-range difference between two editor strings.
 *
 * Y.Text and Obsidian's editor both index UTF-16 code units, so the same indices can be used on
 * both sides. A single replace range is enough for normal typing/paste and keeps remote-applied
 * transactions small.
 */
export function diffRange(current: string, next: string): DiffPatch | null {
  if (current === next) return null
  let prefix = 0
  const maxPrefix = Math.min(current.length, next.length)
  while (prefix < maxPrefix && current[prefix] === next[prefix]) prefix += 1
  let suffix = 0
  const maxSuffix = Math.min(current.length, next.length) - prefix
  while (
    suffix < maxSuffix &&
    current[current.length - 1 - suffix] === next[next.length - 1 - suffix]
  ) {
    suffix += 1
  }
  return {
    index: prefix,
    deleteCount: current.length - prefix - suffix,
    insert: next.slice(prefix, next.length - suffix),
  }
}

/** Map an old-document offset through a replace operation. */
export function mapOffset(offset: number, patch: DiffPatch): number {
  const end = patch.index + patch.deleteCount
  if (offset <= patch.index) return offset
  if (offset >= end) return offset + patch.insert.length - patch.deleteCount
  return patch.index + patch.insert.length
}
