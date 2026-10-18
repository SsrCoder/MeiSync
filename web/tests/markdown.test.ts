import { describe, expect, it } from 'vitest'
import { editorSchema } from '../src/meisync/schema'
import {
  markdownToDoc,
  parseInline,
  parseMarkdownToJSON,
  posToTextIndex,
  serializeDoc,
  textIndexToPos,
} from '../src/meisync/markdown'

const schema = editorSchema()

const roundTrip = (markdown: string) => serializeDoc(markdownToDoc(markdown, schema), false).text

describe('markdown codec', () => {
  it('parses the basic block grammar', () => {
    const json = parseMarkdownToJSON('# Title\n\nplain *text*\n- a\n- b\n1. one\n> quote\n```rust\nlet x = 1;\n```\n---')
    expect(json.content.map((node) => node.type)).toEqual([
      'heading',
      'paragraph',
      'bulletList',
      'orderedList',
      'blockquote',
      'codeBlock',
      'horizontalRule',
    ])
    const code = json.content.find((node) => node.type === 'codeBlock')
    expect(code?.attrs).toEqual({ language: 'rust' })
    expect(code?.content?.[0]?.text).toBe('let x = 1;')
  })

  it('nests lists by indentation', () => {
    const doc = markdownToDoc('- a\n  - b\n    - c\n- d', schema)
    expect(doc.toJSON()).toMatchObject({
      type: 'doc',
      content: [
        {
          type: 'bulletList',
          content: [
            {
              type: 'listItem',
              content: [
                { type: 'paragraph', content: [{ type: 'text', text: 'a' }] },
                {
                  type: 'bulletList',
                  content: [
                    {
                      type: 'listItem',
                      content: [
                        { type: 'paragraph', content: [{ type: 'text', text: 'b' }] },
                        {
                          type: 'bulletList',
                          content: [{ type: 'listItem', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'c' }] }] }],
                        },
                      ],
                    },
                  ],
                },
              ],
            },
            { type: 'listItem', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'd' }] }] },
          ],
        },
      ],
    })
  })

  it('keeps marks', () => {
    expect(parseInline('plain **bold** _italic_ *also* ~~gone~~ `code`')).toEqual([
      { type: 'text', text: 'plain ' },
      { type: 'text', text: 'bold', marks: [{ type: 'bold' }] },
      { type: 'text', text: ' ' },
      { type: 'text', text: 'italic', marks: [{ type: 'italic' }] },
      { type: 'text', text: ' ' },
      { type: 'text', text: 'also', marks: [{ type: 'italic' }] },
      { type: 'text', text: ' ' },
      { type: 'text', text: 'gone', marks: [{ type: 'strike' }] },
      { type: 'text', text: ' ' },
      { type: 'text', text: 'code', marks: [{ type: 'code' }] },
    ])
  })

  // The important property: whatever lands in `Y.Text("content")` must parse back into a
  // document that serialises to exactly the same text, otherwise two clients would ping-pong
  // rewrites of each other's edits.
  const stable = [
    '',
    '# Heading\nsecond line',
    '## Sub _heading_',
    '- one\n- two with **bold**\n  - nested\n- three',
    '1. first\n2. second',
    '> quoted line\n> second paragraph',
    '```\nplain code\n```',
    '```js\nconst a = 1;\n```',
    '---',
    'trailing hard break\\\ncontinuation',
    'literal \\# not a heading',
    'literal \\- not a list',
    '1\\. not a list',
    'text with \\`backticks\\` and \\*stars\\*',
    '# Heading\n- list\n> quote\n```py\ncode\n```',
    'a\nb\nc',
    'multi   spaces   kept',
    'unicode: 中文 · emoji 🎉',
  ]

  it.each(stable)('round trips %j', (markdown) => {
    expect(roundTrip(markdown)).toBe(markdown)
  })

  // `*italic*` is accepted as input but normalised to `_italic_` on the way out.
  it('drops blank lines between blocks', () => {
    expect(roundTrip('# Title\n\n- list\n\n> quote')).toBe('# Title\n- list\n> quote')
    expect(roundTrip('> quoted line\n> \n> second paragraph')).toBe('> quoted line\n> second paragraph')
  })

  it('normalises `*italic*` to `_italic_`', () => {
    expect(roundTrip('a *word* here')).toBe('a _word_ here')
  })

  it('is idempotent for arbitrary documents', () => {
    const doc = markdownToDoc('# t\n- a\n  - b\n> q\n```x\ncode\n```\n---', schema)
    const once = serializeDoc(doc).text
    const twice = serializeDoc(markdownToDoc(once, schema), false).text
    expect(twice).toBe(once)
  })

  it('serializes an empty document as an empty string', () => {
    expect(serializeDoc(markdownToDoc('', schema), false).text).toBe('')
    expect(serializeDoc(schema.topNodeType.createAndFill()!, false).text).toBe('')
  })

  it('escapes document content that looks like syntax', () => {
    const doc = markdownToDoc('\\* not bold \\*', schema)
    expect(serializeDoc(doc, false).text).toBe('\\* not bold \\*')
    const heading = markdownToDoc('\\# literal', schema)
    expect(heading.toJSON().content?.[0].type).toBe('paragraph')
  })
})

describe('position mapping', () => {
  const markdown = '# Title\n\nfirst line\n\n- item one\n- item two'
  const doc = markdownToDoc(markdown, schema)
  const { positions, text } = serializeDoc(doc)

  it('maps every block start to its exact text offset', () => {
    for (const entry of positions) {
      const snippet = doc.textBetween(entry.pos, Math.min(entry.pos + 12, doc.content.size), '\n')
      expect(text.slice(entry.index, entry.index + snippet.length)).toBe(snippet)
    }
  })

  it('is monotonic in both directions', () => {
    for (let i = 1; i < positions.length; i++) {
      expect(positions[i].pos).toBeGreaterThan(positions[i - 1].pos)
      expect(positions[i].index).toBeGreaterThan(positions[i - 1].index)
      expect(positions[i].index).toBeLessThanOrEqual(text.length)
    }
  })

  it('round trips a position/index pair', () => {
    for (const entry of positions) {
      expect(textIndexToPos(positions, entry.index)).toBe(entry.pos)
      expect(posToTextIndex(positions, entry.pos)).toBe(entry.index)
    }
  })

  it('finds the text offset of a block', () => {
    const index = text.indexOf('item two')
    const pos = textIndexToPos(positions, index)
    expect(doc.textBetween(pos, pos + 'item two'.length, '\n')).toBe('item two')
  })
})