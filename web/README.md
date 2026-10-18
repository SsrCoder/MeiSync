# MeiSync web client — Tiptap + Yjs

A browser client built **for testing a MeiSync node**: it edits a file through YSync and, from the
same tab, drives the WebDAV/JSON front ends against the same YDoc, so you can watch all three
protocols converge.

```bash
# 1. a node
cargo run --release --bin meisync -- --data ./data --bind 127.0.0.1:8080

# 2. this client
cd web
npm install
npm run dev            # http://127.0.0.1:5173
MEISYNC_TARGET=http://127.0.0.1:8080 npm run dev   # different node
```

Log in (or press **Register**, which also provisions `/private/<user>` and the `<user>` bucket),
pick a path such as `/private/alice/notes/todo.md` and start typing.

## Why the editor is *not* `@tiptap/extension-collaboration`

MeiSync stores every file as one plain **`Y.Text("content")`** (§6). Tiptap's collaboration
extension is built on y-prosemirror and keeps the document in a `Y.XmlFragment`, which syncs
perfectly over YSync and then *materialises as an empty file* over WebDAV / S3. So this client
projects the editor onto the shared text instead:

```text
  ProseMirror doc ── markdown codec ──► Y.Text("content") ── ysync ──► MeiSync
        ▲                                          │
        └──────────── markdown codec ──────────────┘
```

Consequences of that choice:

* **Markdown, one block per line.** What you type is what `GET /dav` and `aws s3 cp` give you;
  `curl`, `sed` or any non-browser client can join the same document.
* `serialize → parse → serialize` is stable (`tests/markdown.test.ts` proves it), so two clients
  never ping-pong rewrites of each other's edits.
* Supported: headings 1–3, bullet/ordered lists (nested by indentation), blockquotes, fenced code
  blocks, rules, hard breaks, and `**bold** _italic_ ~~strike~~ `code``. `*italic*` is accepted
  and normalised to `_italic_`. Everything else is a paragraph.
* No links, no underline, no tables — a node without a codec round trip is never allowed into
  the schema (`src/meisync/schema.ts`).

## Layout

| file | purpose |
|------|---------|
| `src/meisync/provider.ts` | hand written YSync provider: sync + awareness framing, `auth denied`, `custom(42, path)`, reconnect with backoff |
| `src/meisync/binding.ts` | binds the editor to `Y.Text("content")`: local→remote splice, remote→editor replace, awareness carets, undo hygiene |
| `src/meisync/markdown.ts` | the markdown ⇄ ProseMirror codec and the position ⇄ text-index map used for carets |
| `src/meisync/schema.ts` | the Tiptap schema (exactly the nodes the codec supports) |
| `src/meisync/api.ts` | `/api` control plane + raw WebDAV verbs (`GET`/`PUT`/`MKCOL`/`MOVE`) |
| `src/meisync/session.ts` | one tab = one `Y.Doc` + one provider, plus tab colours |
| `src/Workspace.tsx`, `src/components/*` | the UI: toolbar, editor, namespace tree, HTTP panels, YSync stats, peers, sync log |

### Provider notes

* **Frame batching.** The node answers a fresh connection with `[sync step1, awareness snapshot]`
  in *one* binary WebSocket message, so the provider keeps decoding until the buffer is drained.
  A client that ignores this never learns about peers that were already in the room.
* **Awareness** is relayed per room and dropped when a connection closes; it is never persisted
  into the YDoc (§10).
* `auth denied` closes the connection without reconnecting (the server ends the session too); the
  client surfaces the reason — e.g. `deleted` after `DELETE /api/files/<path>`.

### Binding notes

* Local edits are written as the **smallest differing range** (common prefix/suffix), so two
  clients editing different places merge instead of overwriting.
* Remote updates are dispatched with `addToHistory: false`: undo never replays somebody else's
  text. The local caret is re-mapped through the text index.
* The **session panel** shows `editor == Y.Text` — that invariant is the thing to watch. If it
  ever turns yellow, the projection and the file on disk have drifted apart.

## Testing

```bash
npm test        # markdown codec round trips + position mapping (30 assertions, no browser)
npm run e2e     # headless end-to-end against a running node
npm run explain -- --target http://127.0.0.1:8080 --user alice --path /private/alice/notes/todo.md
npm run explain -- --table     # just the content-type -> sync-mode rule
npm run typecheck
```

`npm run e2e -- --target http://127.0.0.1:8080` uses the *same* provider and codec as the browser
and checks 23 things: registration, two clients converging, concurrent edits merging, awareness
relay (including the late-joiner snapshot and the cleanup on close), WebDAV materialisation,
`PUT`/`MOVE` reaching connected clients, `custom(42, path)` on rename, `auth denied` on delete and
404 for another user's private file.

`npm run explain` answers "why does *this* file show up as base64?": the node resolves the
effective content type from the request `Content-Type` **and** the file extension (a specific
declared type wins; a generic `application/octet-stream` / `application/x-www-form-urlencoded`
defers to the extension), and that decides whether `Y.Text("content")` holds the bytes verbatim
(incremental sync) or base64 (full sync).

### Manual multi-client check

1. Open the client, `?path=/private/alice/notes/todo.md`.
2. **New tab** (adds `?me=<random>`, so the second tab gets its own colour and name).
3. Type in either tab — the other one follows, the peer list shows both carets, and the
   *materialised bytes* box on the left shows what the server hands to WebDAV/S3 clients.
4. Type in the *PUT* box and press **PUT /dav**: the editor updates through YSync without a reload.
5. Delete the file from another tab: the open clients report `access denied: deleted`.

## Known limitations

* Binary files (`sync_mode: "full"`) are base64 inside the same text. That happens when the
  effective content type is not text-ish, i.e. when a *specific* binary `Content-Type` is declared
  or when a generic one (`application/octet-stream`, …) meets an unknown/binary extension — the
  node resolves the type from the request header **and** the file name. The client shows a warning
  and still displays the raw text, but editing such a file would rewrite the encoding.
* The markdown projection is one line per block, so blank lines between blocks are dropped on
  save; a table, an image or a nested blockquote is flattened to its text.
* Merging relies on single-splice range replacement: two clients editing the *same* characters at
  the same moment interleave the way any text CRDT does, but there is no character-level retry
  that would rebuild lost intent.
* The node ships no CORS layer, so the dev server proxies `/api`, `/dav`, `/s3` and `/ysync`. For
  a real deployment the client would talk to the node's origin directly.