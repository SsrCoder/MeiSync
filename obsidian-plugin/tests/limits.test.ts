import { describe, expect, it } from 'vitest'
import { effectiveUploadLimit } from '../src/sync/limits'

describe('upload limits', () => {
  it('uses the client limit until the node advertises one', () => {
    expect(effectiveUploadLimit(32, 0)).toBe(32 * 1024 * 1024)
    expect(effectiveUploadLimit(32, Number.NaN)).toBe(32 * 1024 * 1024)
  })

  it('prefers the smaller of client and server limits', () => {
    // Default node limit (64 MB) is larger than the default client limit (32 MB).
    expect(effectiveUploadLimit(32, 64 * 1024 * 1024)).toBe(32 * 1024 * 1024)
    // A node configured with --max-body-mb=8 wins over the 32 MB client default.
    expect(effectiveUploadLimit(32, 8 * 1024 * 1024)).toBe(8 * 1024 * 1024)
    // A raised client limit is still capped by the node.
    expect(effectiveUploadLimit(128, 64 * 1024 * 1024)).toBe(64 * 1024 * 1024)
  })

  it('clamps nonsense client settings', () => {
    // 0 / missing falls back to the 32 MB default, negatives to the 1 MB floor.
    expect(effectiveUploadLimit(0, 0)).toBe(32 * 1024 * 1024)
    expect(effectiveUploadLimit(-5, 0)).toBe(1 * 1024 * 1024)
  })
})
