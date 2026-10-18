/**
 * Markdown <-> ProseMirror codec for the MeiSync web client.
 *
 * Why markdown? MeiSync stores every file as one plain `Y.Text("content")` (§6). Whatever a
 * client puts into that text is what WebDAV `GET` / S3 `GetObject` materialise and what
 * `aws s3 cp` writes to disk. A markdown-flavoured projection therefore buys us three things:
 *
 *   1. the document survives a round trip through the server unchanged,
 *   2. materialised bytes stay human readable,
 *   3. non-browser clients (`curl`, `cat`, `sed`) can take part in the same document.
 *
 * The grammar is deliberately small — exactly the nodes enabled in `schema.ts`:
 *
 *   # .. ### heading      - item / 1. item (nesting via indentation)
 *   > quote               ``` fenced code block
 *   --- rule              **bold** _italic_ ~~strike~~ `code` + hard break (`\` at EOL)
 *
 * Anything else stays a paragraph, which is what makes `serialize -> parse -> serialize` stable
 * (proved by tests/markdown.test.ts).
 */
import type { Node as PMNode, Schema } from 'prosemirror-model'

export interface PositionEntry {
  /** ProseMirror position (an index into `doc.content`). */
  pos: number
  /** Index into the serialized text, i.e. an index into `Y.Text("content")`. */
  index: number
}

export interface SerializedDoc {
  text: string
  /** Ascending by `index`; maps editor positions <-> Y.Text indices (remote cursors). */
  positions: PositionEntry[]
}

export type RawNode = {
  type: string
  attrs?: Record<string, unknown>
  content?: RawNode[]
  text?: string
  marks?: { type: string }[]
}

// --------------------------------------------------------------------------------------
// serialization
// --------------------------------------------------------------------------------------

/** Escaped everywhere, so document content can never be mistaken for markdown syntax. */
const ALWAYS_ESCAPED = new Set(['\\', '`', '*', '_', '~', '#', '>', '-', '+'])

interface MarkWrapper {
  name: string
  open: string
  close: string
}

/** Innermost first: `code` content is never re-scanned for further markers. */
const MARK_WRAPPERS: MarkWrapper[] = [
  { name: 'code', open: '`', close: '`' },
  { name: 'bold', open: '**', close: '**' },
  { name: 'strike', open: '~~', close: '~~' },
  { name: 'italic', open: '_', close: '_' },
  { name: 'italic', open: '*', close: '*' },
]

function escapeText(text: string): string {
  let out = ''
  for (const char of text) {
    if (char === '\n') continue
    out += ALWAYS_ESCAPED.has(char) ? `\\${char}` : char
  }
  return out
}

/** First line of a block: also hide `1.` style list markers and code fences. */
function escapeLineStart(text: string): string {
  const dotted = text.replace(/^(\s*\d+)\./, '$1\\.')
  if (dotted.startsWith('```') || dotted.startsWith('~~~')) return `\\${dotted}`
  return dotted
}

class Writer {
  readonly positions: PositionEntry[] = []
  private readonly lines: string[] = []
  private index = 0

  constructor(private readonly withPositions: boolean) {}

  /** Write one block line; `prefix` carries list markers / heading hashes. */
  line(pos: number, indent: string, prefix: string, body: string) {
    if (this.withPositions) {
      this.positions.push({ pos, index: this.index + indent.length + prefix.length })
    }
    const text = indent + prefix + body
    this.lines.push(text)
    this.index += text.length + 1 // the '\n' that joins this line with the next
  }

  /** Write a line that carries no position information (code fences, quote continuations). */
  raw(indent: string, body: string) {
    const text = indent + body
    this.lines.push(text)
    this.index += text.length + 1
  }

  finish(): SerializedDoc {
    const text = this.lines.length === 0 ? '' : this.lines.join('\n')
    for (const entry of this.positions) {
      if (entry.index > text.length) entry.index = text.length
    }
    return { text, positions: this.positions }
  }
}

/** Serialize the whole document into `Y.Text("content")` plus a position alignment map. */
export function serializeDoc(doc: PMNode, withPositions = true): SerializedDoc {
  const writer = new Writer(withPositions)
  doc.forEach((child, offset) => writeBlock(child, offset, writer, ''))
  return writer.finish()
}

function writeBlock(node: PMNode, posBefore: number, writer: Writer, indent: string): void {
  switch (node.type.name) {
    case 'paragraph':
      writeInline(node, posBefore + 1, writer, indent, '')
      break
    case 'heading': {
      const level = Math.min(Math.max(Number(node.attrs.level) || 1, 1), 6)
      writeInline(node, posBefore + 1, writer, indent, `${'#'.repeat(level)} `)
      break
    }
    case 'codeBlock': {
      const language = node.attrs.language ? String(node.attrs.language) : ''
      writer.raw(indent, '```' + language)
      const body = node.textContent.replace(/\n$/, '')
      for (const line of body.split('\n')) writer.raw(indent, escapeText(line))
      writer.raw(indent, '```')
      break
    }
    case 'blockquote': {
      const inner = new Writer(false)
      node.forEach((child, offset) => writeBlock(child, posBefore + 1 + offset, inner, ''))
      const lines = inner.finish().text.split('\n')
      lines.forEach((line, index) => {
        if (index === 0) writer.line(posBefore + 1, indent, '> ', line)
        else writer.raw(indent, line === '' ? '>' : `> ${line}`)
      })
      break
    }
    case 'horizontalRule':
      writer.line(posBefore, indent, '', '---')
      break
    case 'bulletList':
    case 'orderedList':
      writeList(node, posBefore, writer, indent)
      break
    default:
      // Unknown container (e.g. `doc` reached through another wrapper): descend into it.
      if (node.childCount > 0) {
        node.forEach((child, offset) => writeBlock(child, posBefore + 1 + offset, writer, indent))
        break
      }
      // Unknown leaf: keep the text so nothing is silently dropped.
      writer.line(posBefore, indent, '', escapeLineStart(escapeText(node.textContent.replace(/\n/g, ' ').trim())))
  }
}

function writeList(node: PMNode, posBefore: number, writer: Writer, indent: string): void {
  const ordered = node.type.name === 'orderedList'
  let number = Number(node.attrs?.start ?? 1) || 1
  node.forEach((item, offset) => {
    const itemPos = posBefore + 1 + offset
    const marker = ordered ? `${number++}. ` : '- '
    const childIndent = indent + ' '.repeat(marker.length)
    let first = true
    item.forEach((child, childOffset) => {
      const childPos = itemPos + 1 + childOffset
      if (first && child.type.name === 'paragraph') {
        writeInline(child, childPos + 1, writer, indent, marker)
      } else {
        writeBlock(child, childPos, writer, childIndent)
      }
      first = false
    })
  })
}

/**
 * Write the inline content of one block. `prefix` carries the block marker (`- `, `# `, `1. `).
 * Hard breaks become a trailing `\` plus a continuation line.
 */
function writeInline(block: PMNode, startPos: number, writer: Writer, indent: string, prefix: string): void {
  const segments: string[][] = [[]]
  block.forEach((child) => {
    if (child.type.name === 'hardBreak') {
      segments.push([])
      return
    }
    if (!child.isText) {
      if (child.textContent) segments[segments.length - 1].push(escapeText(child.textContent))
      return
    }
    segments[segments.length - 1].push(wrapMarks(child))
  })

  // A hard break is a line that ends with an odd backslash; the parser re-joins it with the
  // continuation line, so `serialize -> parse` gives the `hardBreak` node back.
  const body = segments
    .map((segment, index) => (index === 0 ? escapeLineStart(segment.join('')) : segment.join('')))
    .join('\\\n')
  // `startPos` is the position of the block's first character, which is what a remote caret
  // index should be mapped to.
  writer.line(startPos, indent, prefix, body)
}

function wrapMarks(textNode: PMNode): string {
  let text = escapeText(textNode.text ?? '')
  // A mark can have several accepted spellings (`_italic_` / `*italic*`); only the first one is
  // used when writing, otherwise a mark would be wrapped twice.
  const applied = new Set<string>()
  for (const wrapper of MARK_WRAPPERS) {
    if (applied.has(wrapper.name)) continue
    if (!textNode.marks.some((mark) => mark.type.name === wrapper.name)) continue
    applied.add(wrapper.name)
    text = `${wrapper.open}${text}${wrapper.close}`
  }
  return text
}

// --------------------------------------------------------------------------------------
// parsing
// --------------------------------------------------------------------------------------

interface Token {
  text: string
  marks: string[]
}

function parseTokens(raw: string, marks: string[]): Token[] {
  const tokens: Token[] = []
  let buf = ''
  let i = 0
  const flush = () => {
    if (buf) tokens.push({ text: buf, marks: [...marks] })
    buf = ''
  }
  while (i < raw.length) {
    if (raw[i] === '\\' && i + 1 < raw.length) {
      buf += raw.slice(i, i + 2)
      i += 2
      continue
    }
    const open = MARK_WRAPPERS.find((wrapper) => raw.startsWith(wrapper.open, i))
    if (open) {
      const closeAt = raw.indexOf(open.close, i + open.open.length)
      // An unterminated marker stays literal text.
      if (closeAt > i + open.open.length) {
        flush()
        tokens.push(...parseTokens(raw.slice(i + open.open.length, closeAt), [...marks, open.name]))
        i = closeAt + open.close.length
        continue
      }
    }
    buf += raw[i]
    i++
  }
  flush()
  return tokens
}

/** Drop markdown escapes. Escapes never span a mark boundary, so this is merge-safe. */
function unescape(text: string): string {
  let out = ''
  for (let i = 0; i < text.length; i++) {
    if (text[i] === '\\' && i + 1 < text.length) {
      out += text[i + 1]
      i++
      continue
    }
    out += text[i]
  }
  return out
}

/** Tokens -> ProseMirror text nodes (ProseMirror needs one uniform mark set per text node). */
function tokensToNodes(tokens: Token[]): RawNode[] {
  const nodes: RawNode[] = []
  let current: { key: string; node: RawNode } | undefined
  for (const token of tokens) {
    const key = token.marks.join(',')
    if (current && current.key === key) {
      current.node.text = (current.node.text ?? '') + token.text
      continue
    }
    const node: RawNode = { type: 'text', text: token.text }
    if (token.marks.length) node.marks = token.marks.map((type) => ({ type }))
    nodes.push(node)
    current = { key, node }
  }
  return nodes.map((node) => ({ ...node, text: unescape(node.text ?? '') }))
}

/** Parse the inline markup of a single line. */
export function parseInline(raw: string): RawNode[] {
  return tokensToNodes(parseTokens(raw, []))
}

const HEADING_RE = /^(\s{0,3})(#{1,6})\s+(.*)$/
const RULE_RE = /^\s{0,3}([-*_])(\s*\1){2,}\s*$/
const QUOTE_RE = /^(\s{0,3})>\s?(.*)$/
const FENCE_RE = /^\s{0,3}(`{3,}|~{3,})\s*([^`\s]*)\s*$/
const ITEM_RE = /^(\s*)([-+*]|\d+[.)])\s+(.*)$/

function isBlank(line: string): boolean {
  return line.trim() === ''
}

function countIndent(line: string): number {
  let i = 0
  while (i < line.length && line[i] === ' ') i++
  return i
}

/** Does the line end with an odd number of backslashes (a hard-break continuation)? */
function endsWithContinuation(line: string): boolean {
  let count = 0
  for (let i = line.length - 1; i >= 0 && line[i] === '\\'; i--) count++
  return count % 2 === 1
}

interface ListItemLine {
  indent: number
  ordered: boolean
  start: number
  text: string
  /** Non-marker lines indented under the item: extra paragraphs of the same `listItem`. */
  extra: string[]
}

function parseBlocks(lines: string[]): RawNode[] {
  const nodes: RawNode[] = []
  let i = 0

  while (i < lines.length) {
    const line = lines[i]
    if (isBlank(line)) {
      i++
      continue
    }

    const fence = FENCE_RE.exec(line)
    if (fence) {
      const marker = fence[1]
      const language = fence[2] || ''
      const body: string[] = []
      i++
      while (i < lines.length && lines[i].trim() !== marker) {
        body.push(unescape(lines[i]))
        i++
      }
      if (i < lines.length) i++ // closing fence
      nodes.push({
        type: 'codeBlock',
        attrs: language ? { language } : {},
        content: body.length ? [{ type: 'text', text: body.join('\n') }] : undefined,
      })
      continue
    }

    if (RULE_RE.test(line)) {
      nodes.push({ type: 'horizontalRule' })
      i++
      continue
    }

    const heading = HEADING_RE.exec(line)
    if (heading) {
      nodes.push({
        type: 'heading',
        attrs: { level: heading[2].length },
        content: parseInline(heading[3]),
      })
      i++
      continue
    }

    if (QUOTE_RE.test(line)) {
      const inner: string[] = []
      while (i < lines.length) {
        const quote = QUOTE_RE.exec(lines[i])
        if (!quote) break
        inner.push(quote[2])
        i++
      }
      nodes.push({ type: 'blockquote', content: parseBlocks(inner) })
      continue
    }

    if (ITEM_RE.test(line)) {
      const items: ListItemLine[] = []
      while (i < lines.length && !isBlank(lines[i])) {
        const item = ITEM_RE.exec(lines[i])
        if (item) {
          const ordered = /^\d/.test(item[2])
          items.push({
            indent: item[1].length,
            ordered,
            start: ordered ? Number.parseInt(item[2], 10) : 1,
            text: item[3],
            extra: [],
          })
          i++
          continue
        }
        // An indented non-marker line continues the current item.
        if (items.length > 0 && countIndent(lines[i]) > 0) {
          items[items.length - 1].extra.push(lines[i].trim())
          i++
          continue
        }
        break // a non-indented line ends the list
      }
      // `items` may switch between bullet/ordered lists or indentation levels; emit one list per
      // level and resume where the previous one stopped.
      let index = 0
      while (index < items.length) {
        const { node, next } = buildListAt(items, index, items[index].indent, items[index].ordered)
        nodes.push(node)
        if (next <= index) break // defensive: never loop forever
        index = next
      }
      continue
    }

    // Plain paragraph, honouring `\` continuations (hard breaks).
    let raw = line
    const content: RawNode[] = []
    while (endsWithContinuation(raw) && i + 1 < lines.length) {
      content.push(...parseInline(raw.slice(0, -1)))
      content.push({ type: 'hardBreak' })
      i++
      raw = lines[i]
    }
    content.push(...parseInline(raw))
    i++
    nodes.push({ type: 'paragraph', content: content.length ? content : undefined })
  }

  return nodes
}

function buildListAt(
  items: ListItemLine[],
  start: number,
  indent: number,
  ordered: boolean,
): { node: RawNode; next: number } {
  const content: RawNode[] = []
  let i = start
  while (i < items.length && items[i].indent === indent && items[i].ordered === ordered) {
    const item = items[i]
    const itemContent: RawNode[] = [{ type: 'paragraph', content: parseInline(item.text) }]
    for (const extra of item.extra) {
      itemContent.push({ type: 'paragraph', content: parseInline(extra) })
    }
    i++
    const nested: ListItemLine[] = []
    while (i < items.length && items[i].indent > indent) {
      nested.push(items[i])
      i++
    }
    if (nested.length > 0) {
      itemContent.push(buildListAt(nested, 0, nested[0].indent, nested[0].ordered).node)
    }
    content.push({ type: 'listItem', content: itemContent })
  }
  return {
    node: {
      type: ordered ? 'orderedList' : 'bulletList',
      attrs: ordered ? { start: items[start]?.start ?? 1 } : {},
      content: content.length
        ? content
        : [{ type: 'listItem', content: [{ type: 'paragraph', content: [] }] }],
    },
    next: i,
  }
}

/** Parse markdown into Tiptap/ProseMirror JSON. */
export function parseMarkdownToJSON(text: string): { type: 'doc'; content: RawNode[] } {
  const lines = text.replace(/\r\n?/g, '\n').split('\n')
  const content = parseBlocks(lines)
  if (content.length === 0) content.push({ type: 'paragraph', content: [] })
  return { type: 'doc', content }
}

/** Parse markdown into a ProseMirror document of `schema`. */
export function markdownToDoc(text: string, schema: Schema): PMNode {
  const json = parseMarkdownToJSON(text) as unknown
  try {
    return schema.nodeFromJSON(json)
  } catch (error) {
    console.warn('[meisync] markdown parse failed, falling back to an empty document', error)
    return schema.nodeFromJSON({ type: 'doc', content: [{ type: 'paragraph' }] })
  }
}

// --------------------------------------------------------------------------------------
// position mapping (editor positions <-> Y.Text indices)
// --------------------------------------------------------------------------------------

/** ProseMirror position -> index in the serialized text. */
export function posToTextIndex(positions: PositionEntry[], pos: number): number {
  if (positions.length === 0) return 0
  let best = positions[0]
  for (const entry of positions) {
    if (entry.pos <= pos) best = entry
    else break
  }
  return best.index
}

/** Index in the serialized text -> ProseMirror position (clamped to the owning block). */
export function textIndexToPos(positions: PositionEntry[], index: number): number {
  if (positions.length === 0) return 0
  let best = positions[0]
  let next: PositionEntry | undefined
  for (let i = 0; i < positions.length; i++) {
    const entry = positions[i]
    if (entry.index <= index) {
      best = entry
      next = positions[i + 1]
    } else break
  }
  const delta = Math.max(0, index - best.index)
  return next ? best.pos + Math.min(delta, Math.max(0, next.pos - best.pos)) : best.pos + delta
}