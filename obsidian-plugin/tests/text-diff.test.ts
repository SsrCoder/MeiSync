import { describe, expect, it } from 'vitest'
import { diffRange, mapOffset } from '../src/sync/text-diff'

describe('text diff', () => {
  it('finds an insertion', () => {
    expect(diffRange('abc', 'abXc')).toEqual({ index: 2, deleteCount: 0, insert: 'X' })
  })

  it('finds a deletion', () => {
    expect(diffRange('abXc', 'abc')).toEqual({ index: 2, deleteCount: 1, insert: '' })
  })

  it('finds a replacement', () => {
    expect(diffRange('hello world', 'hello Yjs')).toEqual({
      index: 6,
      deleteCount: 5,
      insert: 'Yjs',
    })
  })

  it('handles multiline and unicode text', () => {
    expect(diffRange('# 标题\nold\n', '# 标题\nnew\n')).toEqual({
      index: 5,
      deleteCount: 3,
      insert: 'new',
    })
  })

  it('returns null for identical text', () => {
    expect(diffRange('same', 'same')).toBeNull()
  })

  it('maps cursor offsets around a replacement', () => {
    const patch = { index: 4, deleteCount: 3, insert: 'xy' }
    expect(mapOffset(0, patch)).toBe(0)
    expect(mapOffset(4, patch)).toBe(4)
    expect(mapOffset(5, patch)).toBe(6)
    expect(mapOffset(8, patch)).toBe(7)
  })
})
