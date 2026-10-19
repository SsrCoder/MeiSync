import { describe, expect, it, vi } from 'vitest'
import {
  applyPatchKeepingViewport,
  type CodeMirrorViewLike,
  type PatchableEditor,
} from '../src/sync/editor-apply'

interface Harness {
  editor: PatchableEditor
  dispatch: ReturnType<typeof vi.fn>
  transaction: ReturnType<typeof vi.fn>
  scrollTo: ReturnType<typeof vi.fn>
}

function harness(overrides: Partial<PatchableEditor> = {}, dispatch?: CodeMirrorViewLike['dispatch']): Harness {
  const dispatchSpy = vi.fn(dispatch)
  const transaction = vi.fn()
  const scrollTo = vi.fn()
  const editor: PatchableEditor = {
    cm: { dispatch: dispatchSpy },
    transaction,
    offsetToPos: (offset: number) => ({ line: 0, ch: offset }),
    getScrollInfo: () => ({ left: 0, top: 420 }),
    scrollTo,
    ...overrides,
  }
  return { editor, dispatch: dispatchSpy, transaction, scrollTo }
}

describe('remote patch application', () => {
  it('dispatches on the CodeMirror view with scrolling turned off', () => {
    const { editor, dispatch, transaction } = harness()
    applyPatchKeepingViewport(editor, { index: 6, deleteCount: 5, insert: 'Yjs' }, 'meisync-ysync')

    expect(dispatch).toHaveBeenCalledTimes(1)
    expect(dispatch).toHaveBeenCalledWith({
      changes: { from: 6, to: 11, insert: 'Yjs' },
      scrollIntoView: false,
      userEvent: 'meisync-ysync',
    })
    // editor.transaction() hard-codes scrollIntoView: true, so it must not be used.
    expect(transaction).not.toHaveBeenCalled()
  })

  it('uses a collapsed range for a pure insertion', () => {
    const { editor, dispatch } = harness()
    applyPatchKeepingViewport(editor, { index: 3, deleteCount: 0, insert: 'X' }, 'meisync-ysync')

    expect(dispatch).toHaveBeenCalledWith({
      changes: { from: 3, to: 3, insert: 'X' },
      scrollIntoView: false,
      userEvent: 'meisync-ysync',
    })
  })

  it('does not write selections itself: CodeMirror maps the cursor through the change', () => {
    const { editor, dispatch } = harness()
    applyPatchKeepingViewport(editor, { index: 0, deleteCount: 0, insert: 'a' }, 'meisync-ysync')

    const spec = dispatch.mock.calls[0][0] as Record<string, unknown>
    expect(spec).not.toHaveProperty('selection')
    expect(spec).not.toHaveProperty('selections')
  })

  it('keeps the viewport when only the public transaction API is available', () => {
    const { editor, transaction, scrollTo } = harness({ cm: undefined })
    applyPatchKeepingViewport(editor, { index: 2, deleteCount: 1, insert: 'X' }, 'meisync-ysync')

    expect(transaction).toHaveBeenCalledWith(
      { changes: [{ from: { line: 0, ch: 2 }, to: { line: 0, ch: 3 }, text: 'X' }] },
      'meisync-ysync',
    )
    expect(scrollTo).toHaveBeenCalledWith(0, 420)
  })

  it('falls back to the public API when the view rejects the change', () => {
    const { editor, transaction } = harness({}, () => {
      throw new Error('range out of bounds')
    })
    applyPatchKeepingViewport(editor, { index: 1, deleteCount: 0, insert: 'b' }, 'meisync-ysync')

    expect(transaction).toHaveBeenCalledTimes(1)
  })
})
