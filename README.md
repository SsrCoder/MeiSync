# MeiSync

**One File = One YDoc.** A single Rust server that exposes every file through three protocols on
top of the *same* Yjs/Yrs document:

```text
                      FileDocument
                           │
                          YDoc  (yrs)
                           │
          ┌────────────────┼────────────────┐
          │                │                │
        WebDAV             S3              YSync
      full file I/O     full file I/O    CRDT sync
                       (materialised)   incremental + full
```

This is the implementation of [`docs/mvp.md`](docs/mvp.md). Every section reference in the code
comments (`§7`, `§21`, …) points at that document.

* YDoc is the **only** source of truth for file content – WebDAV `PUT` and S3 `PutObject` never
  touch the filesystem, they turn the body into a YDoc transaction.
* One logical YDoc per `FileId`; move / rename only change `parent_id` + `name`.
* Persistence is SQLite (metadata, auth, ACL, buckets) + local filesystem
  (`snapshot` + append-only `updates/` log with compaction).
* Single node, no PostgreSQL / Redis / Kafka / external S3.

---

## Quick start

```bash
cargo build --release

# 1. start a node
./target/release/meisync --data ./data --bind 127.0.0.1:8080

# 2. create an account
curl -sX POST localhost:8080/api/auth/register \
     -H 'content-type: application/json' \
     -d '{"username":"alice","password":"secret123"}'
# -> {"token":"...","user":{"id":"...","username":"alice",...}}

# 3. use it from every protocol
TOKEN=...   # the token from step 2

# WebDAV
curl -X MKCOL  -H "Authorization: Bearer $TOKEN" localhost:8080/dav/private/alice/notes
curl -X PUT    -H "Authorization: Bearer $TOKEN" --data-binary '# TODO' \
     localhost:8080/dav/private/alice/notes/todo.md
curl           -H "Authorization: Bearer $TOKEN" \
     localhost:8080/dav/private/alice/notes/todo.md

# S3 (path style; the bucket is your home namespace)
curl -X PUT    -H "Authorization: Bearer $TOKEN" --data-binary 'hello' \
     localhost:8080/s3/alice/hello.txt
curl           -H "Authorization: Bearer $TOKEN" localhost:8080/s3/alice/hello.txt

# YSync (y-websocket compatible framing)
ws://127.0.0.1:8080/ysync/private/alice/notes/todo.md?token=$TOKEN
```

`meisync --help` lists every option (`--public-default-write`, `--max-cache-documents`,
`--idle-unload-secs`, `--compact-after-updates`, `--compact-after-bytes`, `--dav-at-root`, …).
`--max-body-mb` (64 by default) is the single upload limit for every protocol — WebDAV `PUT`,
S3 `PutObject`, `PUT /api/files` and `PUT /api/sync/file` — and is advertised to clients as
`max_body_bytes` in `/api/sync/info` and the sync handshake.

### Docker

```bash
docker build -t meisync .
docker run -d --name meisync -p 8080:8080 -v meisync-data:/data meisync
```

The image runs as the unprivileged user `meisync` (uid 10001) and keeps the SQLite database and
YDoc snapshots under the `/data` volume. Everything is configured through CLI flags or
`MEISYNC_*` environment variables (`MEISYNC_DATA_DIR`, `MEISYNC_BIND`, `MEISYNC_ADMIN_USER`,
`MEISYNC_ADMIN_PASSWORD`); appended arguments override the defaults, e.g.
`docker run … meisync --public-default-write`.

`.github/workflows/docker.yml` builds the image and pushes `$DOCKERHUB_USERNAME/meisync` (override
with the `DOCKER_IMAGE` repository variable). It needs the `DOCKERHUB_USERNAME` and
`DOCKERHUB_TOKEN` repository secrets; `v*` / `server-v*` tags additionally publish semver tags,
and pull requests only build without pushing.

### Test client (Tiptap + Yjs)

```bash
cd web && npm install && npm run dev      # http://127.0.0.1:5173
```

`web/` is a browser client written for exercising a node: a Tiptap editor bound to
`Y.Text("content")` (see `web/README.md` for why it is not
`@tiptap/extension-collaboration`), plus panels that drive `/api`, WebDAV `GET`/`PUT`/`MOVE`
against the same file, so a single tab can watch YSync, WebDAV and the JSON API converge. Two
tabs on the same path are a collaborative pair; `npm test` covers the markdown codec and
`npm run e2e` runs a headless 23-check protocol/convergence suite against a live node.

---

## Workspace layout (§31)

```text
crates/
  core/         FileDocument, FileId/Name/ParentId model, User, ACL, PermissionService
  persistence/  SQLite repositories + LocalDocumentStore (DocumentStore trait, §32)
  document/     DocumentManager: FileID→YDoc, cache, rooms, lifecycle, persistence (§21–§24)
  auth/         Argon2 password hashing, session tokens (§11)
  api/          FileService (§33), AppState, /api control plane + Obsidian sync API
  admin/        /api/admin control plane and the embedded /admin console
  webdav/       GET PUT DELETE HEAD PROPFIND MKCOL MOVE COPY (§7)
  s3/           ListBuckets / ListObjects / Get / Put / Delete / Head + SigV4 (§8, §30)
  ysync/        WebSocket Yjs/Yrs sync protocol + awareness (§9, §10)
obsidian-plugin/  Obsidian client: manifest reconciliation + ETag conflicts + realtime feed
web/           Tiptap + Yjs browser test client for the three protocols
server/         the `meisync` binary and router wiring (§35)
```

### The three access paths converge

```text
WebDAV PUT / S3 PutObject ─┐
                           ├─► FileService ─► PermissionService ─► DocumentManager ─► YDoc
YSync update  ────────────┘                                            │
                                                                   ┌────┴─────┐
                                                              persistence   broadcast
```

`replace_content()` (WebDAV/S3) and `apply_update()` (YSync) both end in the same
`DocumentHandle`, which is the only place a YDoc may change. Protocol frontends never touch
storage and never implement permission rules themselves.

---

## Namespace (§4)

```text
/
├── private/
│   ├── alice/          # created on registration, visibility: private
│   └── bob/
└── public/             # shared collections, visibility: public
```

* Registration creates `/private/<username>` **and** an S3 bucket named `<username>` that points
  at it, so `s3://alice/notes/todo.md` is exactly `/private/alice/notes/todo.md`.
* Children of `/private` are provisioned by the server at registration: clients cannot create
  them, so nobody can occupy `/private/<name>` before its owner signs up.
* Top level collections are buckets: `CreateBucket` (`PUT /s3/<name>`) makes a new one.
* Paths are not identity: `MOVE`/`COPY` change `parent_id`/`name` only; the `FileId` and its YDoc
  stay untouched.
* `PUT`/S3 keys create missing intermediate collections implicitly (S3 has no directories).

## Permissions (§12, §13)

| visibility | read | sync | write / delete |
|------------|------|------|---------------|
| `private`  | owner | owner | owner |
| `public`   | any authenticated user | any authenticated user | owner, or an explicit ACL grant |

* New children inherit the visibility of their collection: files created in `/public/...` are
  public, files inside `/private/<user>` stay private.
* `/public` is a shared drop box: every authenticated user may create files there.
* Top level collections (`/`, buckets) may be created by every authenticated user.

* `--public-default-write` flips public files to "any authenticated user may write".
* Grants live in the `acl` table: `POST /api/acl/{file_id}` with
  `{"username":"bob","permissions":["write","sync"]}`.
* Private files of other users answer `404`, never `403`, so the namespace does not leak.

## YDoc content model (§6)

Every document is `Y.Text("content")`. Server metadata (owner, visibility, ACL, path, timestamps)
stays in SQLite and never enters the YDoc.

**How the encoding is chosen** — the *effective* content type of a write decides, and it is
resolved from the request `Content-Type` **and** the file extension:

| request `Content-Type` | name | effective type | sync mode |
|---|---|---|---|
| specific (`text/markdown`, `image/png`, …) | any | the request header | by effective type |
| `application/octet-stream` (Apifox, S3 SDK on an unknown extension) | `notes.md` | `text/markdown` | incremental |
| `application/octet-stream` | `logo.png` | `image/png` | full |
| `application/x-www-form-urlencoded` (`curl --data-binary` default) | `notes.md` | `text/markdown` | incremental |
| `application/octet-stream` | `blob` (no extension) | `application/octet-stream` | full |

A specific declared type always wins; a *generic* one carries no information about the payload,
so the extension decides. Without this, `curl -X PUT --data-binary notes.md` (which sends
`application/x-www-form-urlencoded`) would store a markdown file as base64 and lose incremental
sync for good — the mode is only ever upgraded (`loaded.rs`, §5.2), never downgraded. The
resolved type is persisted and reported by `GET /dav`, `PROPFIND` and the S3 responses.

* Text-ish content types (text/\*, json, yaml, toml, xml, csv, source code, …) are stored as
  UTF-8 text and use **incremental sync** (`snapshot` + update log).
* Everything else (binary, images, archives, unknown types) is stored base64 encoded inside the
  same text root and uses **full-state sync**: the server keeps only the current snapshot
  (§5.2). Both are decoded on materialisation, so `GET` returns the original bytes.

## Persistence (§14–§20)

```text
data/
├── meisync.db                  SQLite: users, sessions, files, acl, ydoc_states, buckets
└── documents/
    └── <file-id>/
        ├── snapshot            encode_state_as_update_v1(state)
        └── updates/
            ├── 00000001        one file per YDoc transaction
            └── 00000002
```

* `snapshot_version`, `update_seq` in `ydoc_states` are **server persistence sequences**, not the
  Yjs logical clock (§19).
* Compaction (`snapshot + updates → new snapshot → drop covered updates`) triggers on
  `max_updates`, `max_update_bytes` or `snapshot_bytes` (§20).
* Recovery = `snapshot` + replay `updates` in sequence order (§17, §18).
* YSync updates are persisted before they are broadcast, so a restart never loses an edit.

## ETag and conditional requests (§25, §26)

`ETag = sha256(encode_state_as_update_v1(state))`, quoted, identical for WebDAV, S3 and YSync.
`If-Match`, `If-None-Match`, `304` and `412` are implemented for both WebDAV and S3, which gives
polling clients cheap change detection and gives writers optimistic concurrency.

## YSync (§9, §10)

Standard `y-websocket` framing, implemented with `yrs::sync::protocol`:

```text
[messageType varuint] [message]
  0 sync      → 0 step1 | 1 step2 | 2 update
  1 awareness → len-prefixed awareness payload
  2 auth      → 0 denied + reason | 1 granted
  3 queryAwareness
```

* Authentication: `Authorization: Bearer <token>`, `?token=<token>`, or
  `Sec-WebSocket-Protocol: bearer.<token>`.
* Authorisation runs through `FileService::open_document` (read + sync permission).
* Awareness is room runtime state: it is relayed to peers and dropped when a connection closes,
  and is **never** persisted into the YDoc.
* File deletion pushes `auth denied("deleted")`; a rename pushes `custom(42, <new path>)`.
* A content-bearing update from a peer without `WRITE` is rejected with an
  `Auth("write access denied")` frame while the connection stays open; an empty diff is not a
  write.

## S3 compatibility (§8, §30)

* A **bucket is a top level collection** of the namespace:
  * `alice` → `/private/alice` (your home, created at registration),
  * any collection you create (`PUT /s3/<name>`, `MKCOL /dav/<name>`) is a bucket as well,
  * `public` → the shared `/public` drop box.
  * The namespace root therefore only holds collections; a file created directly under `/` is
    refused (409), because `GET /s3/<name>` is a bucket listing and could never reach it.
* Path-style addressing, both under `/s3` **and at the endpoint root** (`--endpoint-url`
  clients expect `GET /` = ListBuckets and `GET /<bucket>/<key>` for objects).
* `ListBuckets`, `ListObjectsV2` (prefix, delimiter → `CommonPrefixes`, `max-keys`,
  `continuation-token` → `NextContinuationToken` paging over the merged key/prefix sequence),
  `DeleteObjects`, `GetObject`, `PutObject`, `DeleteObject`, `HeadObject`, `HeadBucket`,
  `CreateBucket`.
* Authentication: bearer token, or real **AWS SigV4** – access key = account name,
  secret = any active session token of that account. Stock clients work unchanged:

```bash
export AWS_ACCESS_KEY_ID=alice
export AWS_SECRET_ACCESS_KEY="$TOKEN"     # any active session token of the account
export AWS_DEFAULT_REGION=us-east-1
aws --endpoint-url http://127.0.0.1:8080 s3 ls                 # ListBuckets
aws --endpoint-url http://127.0.0.1:8080 s3 cp s3://alice/notes/todo.md ./todo.md
```

Verified end to end with the AWS CLI v2 against a running node: `s3 ls`, `s3 ls s3://<bucket>`,
`s3 cp` (upload *and* download), `s3 sync`, `s3 rm`, `s3api head-object` — including objects that
were uploaded over WebDAV, byte for byte.

## API control plane

| method | path | purpose |
|--------|------|---------|
| `GET`  | `/api/health`, `/api/status` | liveness / runtime statistics |
| `POST` | `/api/auth/register`, `/api/auth/login`, `/api/auth/logout` | accounts and tokens |
| `GET`  | `/api/me` | current user |
| `GET`  | `/api/files?depth=n`, `/api/files/<path>` | metadata, listings (`depth=0|1|infinity`) |
| `PUT`  | `/api/files/<path>?visibility=public` | create/replace content |
| `DELETE` | `/api/files/<path>` | recursive delete |
| `GET`  | `/api/document/<path>` | the full `FileDocument`: metadata + materialised YDoc |
| `GET`/`POST`/`DELETE` | `/api/acl/<file_id>` | read / grant / revoke permissions |

---

## Admin console

The node ships with an embedded administration console at **`http://127.0.0.1:8080/admin`**. It
needs no build step or separate frontend deployment: the HTML/CSS/JS is compiled into the binary.

```bash
# recommended: pin an administrator before the first start
MEISYNC_ADMIN_USER=admin MEISYNC_ADMIN_PASSWORD='change-me'   ./target/release/meisync --data ./data --bind 127.0.0.1:8080
```

If no administrator is configured, the **first registered account** is promoted automatically
(disable this with `--first-user-admin=false`). An existing account can also be promoted by
setting `MEISYNC_ADMIN_USER` to its username; `MEISYNC_ADMIN_PASSWORD` is only used when that
account does not exist yet.

The console covers:

* **Overview** – users, files, content bytes, active sessions, cached YDocs, disk usage and recent
  audit events.
* **Users** – create accounts, promote/demote `admin`, disable/enable, reset passwords (which
  revokes all sessions) and delete an account with an optional purge of `/private/<username>`.
* **Files** – browse every namespace without bypassing the server on disk, preview text/binary
  content and delete a file or collection recursively.
* **Sessions** – list active bearer tokens and revoke one immediately.
* **Sync devices** – every Obsidian vault that completed a handshake, with platform, plugin
  version and last-seen time.
* **Audit log** – logins, admin actions, account changes, device registrations and file deletes.
* **Server** – read-only runtime configuration, storage usage, capabilities and endpoint index.

Admin API (all endpoints except `login` require an `admin` bearer token):

| method | path | purpose |
|--------|------|---------|
| `POST` | `/api/admin/login`, `/api/admin/logout` | admin session |
| `GET`  | `/api/admin/me`, `/api/admin/overview`, `/api/admin/status` | identity and runtime picture |
| `GET`/`POST` | `/api/admin/users` | list / create accounts |
| `PATCH`/`DELETE` | `/api/admin/users/{id}` | role, disabled flag, password reset, delete/purge |
| `GET`  | `/api/admin/sessions`, `/api/admin/devices`, `/api/admin/audit` | activity views |
| `DELETE` | `/api/admin/sessions/{id}`, `/api/admin/devices/{user_id}/{device_id}` | revoke / forget |
| `GET`/`DELETE` | `/api/admin/files` | browse / delete any namespace path |
| `GET`  | `/api/admin/file?path=...` | materialised content of one file |

The admin role is stored on the account row; disabling an account invalidates its tokens on the
next request. Session rows carry a masked prefix and an opaque id, never the bearer token itself.
The console itself is a static asset; only the API is protected. See
[`docs/admin.md`](docs/admin.md) for the complete reference.

---

## Obsidian sync plugin

[`obsidian-plugin/`](obsidian-plugin/) is a **hybrid** Obsidian client built for this node:

* **Open Markdown notes use real YSync/Yjs**: the shared `Y.Text("content")` is edited
  character-by-character through `ws://.../ysync/<path>`, alongside the web/Tiptap client and
  other hybrid Obsidian clients.
* **Closed notes, attachments and `.obsidian` config use file-level sync**: recursive
  **manifest**, ETag-conditional whole-file upload/download, **move**/**mkdir**, conflict copies
  and a **WebSocket change feed**. The feed also announces YSync edits once they are materialised,
  so a closed copy of a note on another device updates immediately instead of waiting for its next
  manifest poll.
* While a note is open, the file-level engine ignores its path, so a whole-file PUT can never
  overwrite a CRDT edit.
* **Device registration** makes each vault visible in the admin console.
* Configurable initial-sync mode, include/exclude rules, attachment size limits and conflict
  strategy.

The hybrid realtime switch lives in the plugin settings; disabling it reverts the whole vault to
file-level sync.

```bash
cd obsidian-plugin
npm install
npm run build          # -> dist/ (main.js + manifest.json + styles.css)
```

Copy the contents of `dist/` into
`<vault>/.obsidian/plugins/meisync-obsidian/`, enable **MeiSync** in Obsidian's community plugins,
then set the server URL and account in the plugin settings. The remote folder defaults to
`obsidian/<vault name>` under `/private/<username>`.

Sync API:

| method | path | purpose |
|--------|------|---------|
| `GET`  | `/api/sync/info` | protocol version and capabilities |
| `POST` | `/api/sync/handshake` | resolve the vault root, register the device |
| `GET`  | `/api/sync/manifest?vault=...` | recursive relative manifest (`ETag` = revision) |
| `GET`/`PUT`/`DELETE` | `/api/sync/file?vault=...&path=...` | content with conditional ETags |
| `POST` | `/api/sync/move`, `/api/sync/mkdir` | rename/move and folder creation |
| `GET`/`POST` | `/api/sync/devices` | list/register sync devices |
| `GET`  | `/api/sync/ws?vault=...` | WebSocket change feed (`ready`, `change`, `resync`) |

The vault root is always interpreted **relative to `/private/<username>`**; absolute paths and
`..` are rejected. The server still enforces the normal owner permissions, so a token can never
address another user's namespace through the sync API. See
[`docs/obsidian-sync.md`](docs/obsidian-sync.md) for the hybrid model, YSync bridge, conflict
rules and protocol details.

### Releases

`.github/workflows/plugin-release.yml` runs on a `plugin-v<version>` tag, checks that the tag
matches `obsidian-plugin/manifest.json`, and publishes a GitHub release containing `main.js`,
`manifest.json` and `styles.css` (the assets Obsidian and BRAT download) plus
`meisync-obsidian-<version>.zip` for manual installs. A manual `workflow_dispatch` builds the zip
as a workflow artifact without creating a release.

```bash
git tag plugin-v0.2.4 && git push origin plugin-v0.2.4
```

---

## Tests

```bash
cargo test --workspace
```

* `crates/core` – permission matrix (private/public/ACL).
* `crates/persistence/tests/storage.rs` – update log ordering, compaction thresholds, SQLite
  repositories (namespace uniqueness, cascade delete, ACL, YDoc state, buckets).
* `crates/document/tests/manager.rs` – one YDoc per file, ETag changes, persistence + replay after
  eviction, compaction, full-state documents, room broadcast, incremental diffs, binary content,
  idle sweeping, deletion.
* `server/tests/webdav.rs` – full lifecycle, ETags/`304`/`412`, PROPFIND, MOVE/COPY, private vs
  public + ACL, recursive delete, UTF-8 paths.
* `server/tests/s3.rs` – buckets/objects, listing with prefix and delimiter, conditional
  GET/`304`/`412`, cross-protocol ETags, continuation-token paging, SigV4 (positive and
  negative).
* `server/tests/security.rs` – no protocol reveals anything without a valid identity (WebDAV, S3
  at `/s3` and at the endpoint root, JSON API, YSync upgrade), and logging out invalidates the
  token immediately.
* `server/tests/ysync.rs` – two real WebSocket clients converging through the sync protocol,
  awareness relay, incremental vs full-state transfer, delete/rename notifications, permission
  rejection and restart persistence.
* `server/tests/admin.rs` – admin login, role gating, user lifecycle (disable/password
  reset/delete+purge), sessions, audit and cross-namespace file browsing.
* `server/tests/sync.rs` – vault provisioning, manifest, conditional ETag conflicts,
  move/delete, device registration, namespace scoping and the WebSocket change feed.
* `obsidian-plugin/tests` – pure planner, path/exclusion and text-diff rules (`npm test`).
* `server/tests/persistence.rs` – restart recovery, compaction, full-sync documents, concurrent
  writes, cache eviction, `/api/status`.
* `server/tests/api.rs` – accounts, tokens, JSON file API, namespace bootstrap, path traversal.

## MVP scope (§38)

Not implemented on purpose: clustering, distributed YDocs, Redis/Kafka, PostgreSQL, external S3
storage, complex ACL, OAuth/OIDC, multi-tenancy, version history UI, full text search. The admin
console is intentionally an operational tool, not a collaborative document editor. The Obsidian
plugin now uses YSync for open Markdown notes and file-level sync for everything else; it does not
try to make attachments or `.obsidian` config collaborative. The abstractions that would carry them (`DocumentStore`, `PermissionService`, `FileService`, `Room`)
are already factored out so a `PostgresDocumentStore` or a distributed room can replace the local
implementation without touching the protocol layers.