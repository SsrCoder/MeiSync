# MeiSync 技术架构设计

**版本：** v0.1
**状态：** Draft / Architecture Baseline
**技术栈：** Rust + Yrs + SQLite + Local Filesystem
**项目定位：** 基于 Yjs/Yrs 的统一文件同步服务

---

# 1. 项目概述

MeiSync 是一个以 **YDoc 为文件状态核心**的轻量级文件同步 Server。

系统同时提供：

* WebDAV 文件访问
* S3-compatible API
* Yjs/Yrs Sync Protocol
* 用户身份认证
* 私有文件与公开文件
* 实时 CRDT 增量同步
* 大文件/不适合增量同步文件的 Yjs 全量状态同步
* 基于 SQLite + 本地文件系统的轻量级持久化

MeiSync 的核心设计原则是：

> **One File = One YDoc**

服务器中的每一个文件对象都对应一个独立的 YDoc。

WebDAV、S3 和 YSync 并不是三套独立的文件存储系统，而是同一个 FileDocument 的不同访问协议。

```text
                         FileDocument
                              │
                             YDoc
                              │
             ┌────────────────┼────────────────┐
             │                │                │
           WebDAV             S3              YSync
             │                │                │
          完整文件           完整文件          Yjs/Yrs Sync
          读 / 写            读 / 写           增量 / 全量
```

---

# 2. 核心设计原则

## 2.1 One File = One YDoc

每一个文件都有独立的 YDoc：

```text
/private/alice/todo.md
        │
        └── YDoc A

/private/alice/config.json
        │
        └── YDoc B

/public/readme.md
        │
        └── YDoc C
```

不同文件之间不存在共享 YDoc。

文件移动、重命名不会创建新的 YDoc。

```text
/notes/todo.md
      ↓
/archive/todo.md
```

本质上只是 FileDocument 的 metadata 发生变化：

```text
parent_id
name
```

YDoc 本身保持不变。

---

## 2.2 YDoc 是文件内容的唯一 Source of Truth

系统中不存在：

```text
WebDAV File
S3 Object
Yjs Document
```

三份独立的数据。

正确模型是：

```text
                    FileDocument
                         │
                        YDoc
                         │
          ┌──────────────┼──────────────┐
          │              │              │
        YSync          WebDAV           S3
          │              │              │
      CRDT Update    Full Snapshot   Full Snapshot
```

WebDAV/S3 的完整文件内容都是从当前 YDoc materialize 得到的。

---

## 2.3 所有写操作最终进入 YDoc

无论写入来源是什么：

```text
YSync Update
WebDAV PUT
S3 PUT
```

最终都必须转换为 YDoc 的状态变化。

```text
YSync Update ─────┐
                  │
WebDAV PUT ───────┼──→ YDoc Transaction
                  │
S3 PUT ───────────┘
                         │
                 ┌───────┴───────┐
                 ▼               ▼
             Persistence     Broadcast
```

这样可以保证：

> Server 永远只有一个文件状态来源。

---

# 3. 文件模型

## 3.1 FileDocument

核心领域对象：

```rust
struct FileDocument {
    id: FileId,
    metadata: FileMetadata,
    doc: yrs::Doc,
}
```

其中：

```rust
struct FileMetadata {
    id: FileId,

    parent_id: Option<FileId>,

    name: String,

    owner_id: UserId,

    visibility: Visibility,

    content_type: Option<String>,

    created_at: DateTime<Utc>,

    updated_at: DateTime<Utc>,
}
```

---

# 4. 文件 Namespace

文件使用树形 Namespace。

例如：

```text
/
├── private/
│   ├── alice/
│   │   ├── todo.md
│   │   └── config.json
│   │
│   └── bob/
│       └── todo.md
│
└── public/
    ├── readme.md
    └── shared/
        └── config.json
```

内部使用：

```text
File ID
Parent ID
Name
```

表示文件路径。

路径不是文件身份。

因此：

```text
/notes/todo.md
```

和：

```text
/archive/todo.md
```

如果只是移动操作，本质上仍然是同一个 FileDocument。

---

# 5. 文件类型与同步能力

MeiSync 不要求用户在创建文件时选择：

```text
普通文件
Yjs 文件
```

所有文件都属于 FileDocument，并支持 YSync。

区别仅在于：

> **YDoc 的同步策略可以不同。**

主要存在两种同步模式。

## 5.0 模式如何决定

写入时（WebDAV `PUT` / S3 `PutObject` / JSON `PUT` / `COPY`）先解析出**有效 Content-Type**：

| 请求 Content-Type | 文件名 | 有效类型 | 模式 |
|---|---|---|---|
| 具体类型（`text/markdown`、`image/png` …） | 任意 | 请求头 | 按有效类型 |
| `application/octet-stream`（Apifox / S3 SDK 未知扩展名） | `notes.md` | `text/markdown` | Incremental |
| `application/octet-stream` | `logo.png` | `image/png` | Full |
| `application/x-www-form-urlencoded`（`curl --data-binary` 默认） | `notes.md` | `text/markdown` | Incremental |
| `application/octet-stream` | `blob`（无扩展名） | `application/octet-stream` | Full |

规则：**具体的声明类型优先；泛用类型（`application/octet-stream`、`binary/octet-stream`、
`application/x-www-form-urlencoded` 等）不携带信息，此时由扩展名决定**。这是必要的，因为大量
客户端（curl、Apifox、几乎所有 S3 SDK）上传文本文件时也会发泛用类型，若只看请求头会把
`notes.md` 存成 base64 并永久失去增量同步。解析结果会写入 `files.content_type`，因此
`GET /dav` 与 S3 响应返回的都是解析后的类型。

## 5.1 Incremental Sync

适合：

* Markdown
* TXT
* JSON
* YAML
* TOML
* 源代码
* 其他适合 CRDT 增量修改的内容

服务器保存：

```text
Snapshot
+
Update Log
```

客户端之间通过 Yjs/Yrs Update 进行增量同步。

---

## 5.2 Full Sync

适合：

* 大型文件
* 二进制文件
* 不适合细粒度 CRDT 修改的文件

仍然使用 Yjs/Yrs Sync Protocol。

区别是服务器不需要长期保存大量细粒度 Update，而主要维护当前完整 YDoc 状态。

客户端同步时：

```text
Current YDoc
      ↓
Encode Full State
      ↓
Yjs Update
      ↓
Client
```

因此：

> **Full Sync 不是“不支持 Yjs”，而是不提供高效的内容增量同步。**

---

# 6. YDoc 内容模型

MVP 首先采用统一的内容模型：

```text
YDoc
└── content
```

对于文本内容：

```text
YDoc
└── Y.Text("content")
```

例如：

```text
YDoc
└── content
    └── "# TODO\n\n- task 1"
```

未来可以扩展为：

```text
YDoc
├── content
└── metadata
```

或者：

```text
YDoc
└── content
      ├── chunks
      └── ...
```

用于更大型的数据。

但是 Server metadata 不放入 YDoc：

```text
owner
visibility
ACL
path
created_at
```

这些属于 Server 管理的数据。

---

# 7. 三种访问协议

## 7.1 WebDAV

WebDAV 提供文件系统风格的访问方式。

支持：

```text
GET
PUT
DELETE
HEAD
PROPFIND
MKCOL
MOVE
COPY
```

核心语义：

### GET

```text
HTTP GET
    ↓
Resolve File
    ↓
Permission Check
    ↓
Load YDoc
    ↓
Materialize
    ↓
Full File Bytes
```

### PUT

```text
HTTP PUT
    ↓
Read Full Body
    ↓
Resolve File
    ↓
Permission Check
    ↓
Load YDoc
    ↓
Replace Current Content
    ↓
YDoc Transaction
    ↓
Persistence
    ↓
Broadcast
```

因此 WebDAV PUT 并不是直接覆盖磁盘文件。

而是：

> **使用完整内容覆盖当前 YDoc 状态。**

---

# 8. S3-compatible API

MeiSync 提供 S3-compatible API。

第一版不要求真正使用外部 S3 Storage。

请求：

```text
S3 Client
    ↓
MeiSync S3 API
    ↓
FileService
    ↓
FileDocument
    ↓
YDoc
```

### GET Object

```text
YDoc
 ↓
Materialize
 ↓
Object Bytes
```

### PUT Object

```text
Object Body
 ↓
Load YDoc
 ↓
Replace Content
 ↓
YDoc Transaction
 ↓
Persistence
 ↓
Broadcast
```

因此 S3 和 WebDAV 对 FileDocument 的语义保持一致。

---

# 9. YSync

YSync 是 MeiSync 的实时同步接口。

协议：

```text
Yjs/Yrs Sync Protocol
```

连接模型：

```text
WebSocket
    ↓
Authenticate
    ↓
Authorize File
    ↓
Join File Room
    ↓
YSync
```

一个 FileDocument 对应一个逻辑 Room：

```text
File A
  ↓
Room A

File B
  ↓
Room B
```

多个客户端可以同时加入同一个 Room。

```text
Alice ─────┐
Bob ───────┼──→ YDoc A
Charlie ───┘
```

---

# 10. Awareness

YSync 除了 CRDT Update，还可以支持 Yjs Awareness。

例如：

```text
Alice:
cursor = position 100

Bob:
cursor = position 200
```

Awareness 不需要作为持久化文件内容。

它属于：

```text
Runtime State
```

生命周期与 WebSocket Connection 绑定。

因此：

```text
YDoc Persistence
```

和：

```text
Awareness
```

完全分离。

---

# 11. Authentication

MVP 使用账号 + Token。

用户模型：

```text
User
├── id
├── username
├── password_hash
└── created_at
```

客户端：

```http
Authorization: Bearer <token>
```

WebSocket：

```text
WebSocket
    ↓
Authenticate
    ↓
User Identity
```

认证与授权分离。

---

# 12. 权限模型

第一版支持两种文件可见性：

```rust
enum Visibility {
    Private,
    Public,
}
```

## Private

```text
/private/{user}/...
```

只有文件 owner 可以访问。

包括：

```text
WebDAV
S3
YSync
```

---

## Public

公开文件可以在整个 MeiSync Server 内共享。

默认：

```text
Authenticated Users
        ↓
Read
        ↓
Write
        ↓
YSync
```

具体 Write 权限仍由 File Permission 层控制。

未来可以扩展：

```text
Read
Write
Delete
Sync
Share
```

---

# 13. Permission Service

协议层不直接判断权限。

统一：

```text
Protocol
   ↓
FileService
   ↓
PermissionService
```

例如：

```rust
trait PermissionService {
    async fn can_read(
        user: &User,
        file: &FileDocument,
    ) -> Result<bool>;

    async fn can_write(
        user: &User,
        file: &FileDocument,
    ) -> Result<bool>;

    async fn can_sync(
        user: &User,
        file: &FileDocument,
    ) -> Result<bool>;
}
```

这样 WebDAV、S3、YSync 不会出现三套权限逻辑。

---

# 14. Persistence

MVP 使用：

```text
SQLite
+
Local Filesystem
```

不依赖：

```text
PostgreSQL
Redis
S3
MinIO
```

系统启动后即可运行。

---

# 15. SQLite 职责

SQLite 保存：

* User
* Session
* File Metadata
* Namespace
* Permission
* YDoc Persistence Metadata

示例：

```sql
CREATE TABLE users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
```

```sql
CREATE TABLE files (
    id TEXT PRIMARY KEY,

    parent_id TEXT,

    name TEXT NOT NULL,

    owner_id TEXT NOT NULL,

    visibility INTEGER NOT NULL,

    content_type TEXT,

    created_at INTEGER NOT NULL,

    updated_at INTEGER NOT NULL,

    UNIQUE(parent_id, name)
);
```

YDoc metadata：

```sql
CREATE TABLE ydoc_states (
    file_id TEXT PRIMARY KEY,

    snapshot_version INTEGER NOT NULL,

    update_seq INTEGER NOT NULL,

    updated_at INTEGER NOT NULL
);
```

---

# 16. Filesystem Persistence

YDoc 数据存放在 filesystem。

推荐：

```text
data/
├── meisync.db
│
└── documents/
    ├── <file-id>/
    │   ├── snapshot
    │   └── updates/
    │       ├── 00000001
    │       ├── 00000002
    │       └── ...
    │
    └── ...
```

这样 SQLite 不需要承载大量 BLOB。

---

# 17. Snapshot

Snapshot 保存某一时刻完整的 YDoc 状态。

```text
YDoc
 ↓
Encode State
 ↓
snapshot
```

恢复：

```text
snapshot
   ↓
YDoc
```

---

# 18. Incremental Update

增量模式额外保存 Update：

```text
snapshot
+
u1
+
u2
+
u3
```

恢复：

```text
Snapshot
    ↓
Apply u1
    ↓
Apply u2
    ↓
Apply u3
    ↓
Current YDoc
```

---

# 19. Persistence Sequence

Update 文件可以拥有 Server-side sequence：

```text
seq = 1
seq = 2
seq = 3
```

需要注意：

> Persistence sequence 不是 Yjs CRDT clock。

两者完全不同。

```text
Persistence Sequence
        ≠
Yjs Logical Clock
```

Persistence sequence 仅用于：

* update 排序
* snapshot compaction
* 数据恢复
* Server 内部管理

---

# 20. Snapshot / Compaction

Update Log 不能无限增长。

因此需要定期：

```text
Snapshot
+
Update Log
      ↓
Compact
      ↓
New Snapshot
      ↓
Delete obsolete updates
```

触发条件可以根据：

```text
Update Count
Update Bytes
Snapshot Size
```

决定。

例如：

```text
updates >= 1000
```

或者：

```text
update_bytes >= 4MB
```

触发 snapshot。

第一版具体阈值通过配置项控制。

---

# 21. DocumentManager

`DocumentManager` 是整个系统的核心组件。

职责：

* FileID → YDoc
* YDoc Load
* YDoc Cache
* YDoc Lifecycle
* Room 管理
* Update 广播
* Persistence
* Snapshot
* Concurrent Access

核心概念：

```text
FileID
  ↓
DocumentManager
  ↓
LoadedDocument
```

可以设计为：

```rust
struct LoadedDocument {
    file_id: FileId,

    doc: yrs::Doc,

    room: Room,

    dirty: bool,

    last_access: Instant,
}
```

---

# 22. YDoc 生命周期

完整生命周期：

```text
              ┌─────────────┐
              │ Not Loaded  │
              └──────┬──────┘
                     │
                     │ first access
                     ▼
              ┌─────────────┐
              │   Loading   │
              └──────┬──────┘
                     │
                     ▼
              ┌─────────────┐
              │    Loaded   │
              └──────┬──────┘
                     │
             ┌───────┴────────┐
             │                │
          WebSocket         File API
             │                │
             └───────┬────────┘
                     │
                     ▼
                Dirty / Active
                     │
                     ▼
                 Persist
                     │
                     ▼
                 Idle
                     │
                     ▼
                  Unload
```

---

# 23. YDoc Cache

Server 不应该永久将所有文件 YDoc 放在内存。

使用 LRU 或类似缓存策略：

```text
DocumentManager
      │
      ▼
  LRU Cache
      │
 ┌────┼────┐
 ▼    ▼    ▼
Doc A Doc B Doc C
```

冷文件可以：

```text
Persist
 ↓
Remove from memory
```

再次访问：

```text
Filesystem
 ↓
Restore
 ↓
YDoc
```

---

# 24. 并发模型

同一个 FileDocument 的所有 YDoc 修改必须经过统一的并发控制。

原则：

> **同一 FileID 在同一时刻只允许一个逻辑 YDoc 实例。**

避免：

```text
Request A
  ↓
YDoc A

Request B
  ↓
YDoc B
```

导致两个内存 YDoc 同时修改同一个文件。

因此：

```text
FileID
  ↓
DocumentManager
  ↓
Single Loaded YDoc
```

---

# 25. WebDAV / S3 完整覆盖的并发语义

例如当前：

```text
Hello
```

客户端 A：

```text
PUT:
Hello A
```

客户端 B：

```text
PUT:
Hello B
```

两次 PUT 都会成为 YDoc Transaction。

因此最终结果取决于它们到达 YDoc Transaction 的顺序。

为了避免客户端无意覆盖，可以支持：

```text
ETag
If-Match
If-None-Match
```

例如：

```http
If-Match: "abc123"
```

如果当前文件版本已经发生变化：

```text
412 Precondition Failed
```

这样 WebDAV/S3 客户端可以进行乐观并发控制。

---

# 26. ETag

ETag 可以由当前 YDoc 状态生成。

例如：

```text
YDoc State
    ↓
State Hash
    ↓
ETag
```

因此：

```http
GET /foo.md
If-None-Match: "abc123"
```

如果 YDoc 没有变化：

```http
304 Not Modified
```

这对于 WebDAV polling 非常重要。

---

# 27. WebDAV / S3 不提供实时通知

WebDAV/S3 的语义：

```text
GET
PUT
DELETE
HEAD
```

它们可以获得最新状态。

但它们本身不会订阅：

```text
YDoc Update
```

因此：

```text
YSync
```

负责：

> 实时通知

而：

```text
WebDAV
S3
```

负责：

> Snapshot Access

这是三种协议最核心的区别。

---

# 28. 文件删除

删除 FileDocument 时：

```text
Permission Check
      ↓
Mark/Delete Metadata
      ↓
Close YDoc Room
      ↓
Persist/Delete Content
      ↓
Broadcast Delete
```

YSync 客户端应该收到文件已经不存在的状态。

具体删除通知可以在应用层协议中实现。

---

# 29. 文件移动 / 重命名

移动：

```text
parent_id
```

变化。

重命名：

```text
name
```

变化。

不会修改：

```text
YDoc
```

因此：

```text
/notes/a.md
      ↓
/archive/a.md
```

仍然使用同一个：

```text
FileID
YDoc
```

---

# 30. S3 Key 与 File Namespace

S3：

```text
bucket/path/to/file.md
```

可以映射为：

```text
Namespace
    ↓
File Path
    ↓
File ID
```

S3 API 不应该绕过 FileService 直接访问 filesystem。

正确流程：

```text
S3 Request
    ↓
Path Resolver
    ↓
FileService
    ↓
FileDocument
```

---

# 31. 模块划分

Rust Workspace 建议：

```text
meisync/
├── Cargo.toml
│
├── crates/
│   ├── core/
│   │   ├── file
│   │   ├── user
│   │   └── permission
│   │
│   ├── document/
│   │   ├── manager
│   │   ├── room
│   │   └── lifecycle
│   │
│   ├── persistence/
│   │   ├── sqlite
│   │   └── filesystem
│   │
│   ├── auth/
│   │
│   ├── ysync/
│   │
│   ├── webdav/
│   │
│   ├── s3/
│   │
│   └── api/
│
└── server/
```

---

# 32. Storage Abstraction

Persistence 应该抽象。

例如：

```rust
trait DocumentStore {
    async fn load_snapshot(
        &self,
        file_id: FileId,
    ) -> Result<Option<Vec<u8>>>;

    async fn append_update(
        &self,
        file_id: FileId,
        update: &[u8],
    ) -> Result<UpdateSeq>;

    async fn save_snapshot(
        &self,
        file_id: FileId,
        snapshot: &[u8],
    ) -> Result<()>;

    async fn delete(
        &self,
        file_id: FileId,
    ) -> Result<()>;
}
```

MVP：

```text
DocumentStore
    ↓
LocalDocumentStore
    ├── SQLite
    └── Filesystem
```

未来可以增加：

```text
PostgresDocumentStore
S3DocumentStore
DistributedDocumentStore
```

而不影响上层。

---

# 33. FileService

所有协议统一进入 FileService。

```rust
trait FileService {
    async fn create(...);

    async fn get(...);

    async fn list(...);

    async fn delete(...);

    async fn move_file(...);

    async fn read_content(...);

    async fn replace_content(...);
}
```

其中：

```text
replace_content()
```

是 WebDAV/S3 PUT 的核心。

```text
apply_update()
```

则是 YSync 的核心。

但两者最终都会操作 DocumentManager。

---

# 34. 完整数据流

## YSync Update

```text
Client
  │
  │ Yjs Update
  ▼
WebSocket
  │
  ▼
Auth
  │
  ▼
Permission
  │
  ▼
DocumentManager
  │
  ▼
YDoc Transaction
  │
  ├──── Persistence
  │
  └──── Broadcast
          │
          ├── Client A
          ├── Client B
          └── Client C
```

---

## WebDAV PUT

```text
WebDAV Client
      │
      │ Full File
      ▼
   WebDAV
      │
      ▼
 FileService
      │
      ▼
DocumentManager
      │
      ▼
YDoc Transaction
      │
      ├──── Persistence
      │
      └──── Broadcast
```

---

## S3 PUT

```text
S3 Client
    │
    │ Full Object
    ▼
 S3 API
    │
    ▼
FileService
    │
    ▼
DocumentManager
    │
    ▼
YDoc Transaction
    │
    ├──── Persistence
    │
    └──── Broadcast
```

---

# 35. MVP 部署形态

第一版只支持单节点：

```text
┌─────────────────────────┐
│        MeiSync          │
│                         │
│ HTTP                    │
│ WebDAV                  │
│ S3                      │
│ WebSocket / YSync       │
│                         │
│ DocumentManager         │
│                         │
│ SQLite                  │
│ Local Filesystem        │
└─────────────────────────┘
```

数据：

```text
./data/
├── meisync.db
└── documents/
```

部署：

```bash
meisync --data ./data
```

不需要：

```text
PostgreSQL
Redis
Kafka
S3
```

---

# 36. 集群扩展方向

MVP 不实现，但架构需要预留。

未来：

```text
             Load Balancer
                   │
       ┌───────────┼───────────┐
       ▼           ▼           ▼
    Server A    Server B    Server C
       │           │           │
       └───────────┼───────────┘
                   │
             Shared Storage
                   │
          ┌────────┴────────┐
          │                 │
      PostgreSQL          Object Storage
```

YDoc Room 需要增加：

```text
Distributed Room
```

以及：

```text
Update Broadcast
```

可能使用：

```text
Redis
NATS
Kafka
```

但不属于 MVP。

---

# 37. 一致性原则

MeiSync 的核心一致性原则：

### 原则 1

YDoc 是当前文件状态的唯一来源。

### 原则 2

任何写操作最终都必须形成 YDoc 状态变化。

### 原则 3

Persistence 必须能够恢复当前 YDoc。

### 原则 4

实时客户端通过 YSync 获取增量/全量状态。

### 原则 5

WebDAV/S3 永远看到的是当前 YDoc materialized 后的完整文件。

---

# 38. MVP 明确不做的事情

第一阶段暂不实现：

* 多节点集群
* 分布式 YDoc
* Redis
* Kafka
* PostgreSQL
* 外部 S3 Storage
* 复杂 ACL
* OAuth/OIDC
* 多租户
* 文件版本历史 UI
* 服务端全文搜索
* 高级对象存储生命周期策略

但是架构必须保证这些能力未来可以加入。

---

# 39. MVP 必须实现的能力

## 用户

* 注册
* 登录
* Token
* 用户身份识别

## 文件

* 创建
* 查询
* 列目录
* 读取
* 完整覆盖
* 删除
* 移动
* 重命名

## 权限

* Private
* Public
* Read
* Write
* Sync

## WebDAV

* GET
* PUT
* DELETE
* HEAD
* PROPFIND
* MKCOL
* MOVE
* COPY

## S3

至少：

* List Buckets
* List Objects
* Get Object
* Put Object
* Delete Object
* Head Object

## YSync

* Authentication
* Authorization
* Yjs/Yrs Sync Protocol
* Awareness
* Incremental Sync
* Full State Sync

## Persistence

* SQLite
* Snapshot
* Update
* Recovery
* Compaction
* Local filesystem

---

# 40. 最终架构

```text
                         ┌──────────────────┐
                         │      Client      │
                         └────────┬─────────┘
                                  │
              ┌───────────────────┼───────────────────┐
              │                   │                   │
              ▼                   ▼                   ▼
           WebDAV                 S3                 YSync
              │                   │                   │
              │             Full Snapshot        Yjs/Yrs Sync
              │             Read / Write        Incremental/Full
              │                   │                   │
              └───────────────────┼───────────────────┘
                                  │
                                  ▼
                       ┌───────────────────┐
                       │    Auth / ACL     │
                       └─────────┬─────────┘
                                 │
                                 ▼
                       ┌───────────────────┐
                       │    FileService    │
                       └─────────┬─────────┘
                                 │
                                 ▼
                       ┌───────────────────┐
                       │ DocumentManager   │
                       │                   │
                       │ FileID → YDoc     │
                       │ Cache             │
                       │ Room              │
                       │ Lifecycle         │
                       └─────────┬─────────┘
                                 │
                                 ▼
                       ┌───────────────────┐
                       │       Yrs         │
                       │                   │
                       │       YDoc        │
                       └─────────┬─────────┘
                                 │
                    ┌────────────┴────────────┐
                    │                         │
                    ▼                         ▼
             DocumentStore              Broadcast
                    │
            ┌───────┴────────┐
            │                │
            ▼                ▼
         SQLite          Filesystem
       Metadata/Auth    Snapshot/Update
```

---

# 41. 核心结论

MeiSync 不是：

```text
WebDAV Server
+
S3 Server
+
Yjs Server
```

三个服务的简单组合。

它实际上是：

> **一个以 FileDocument/YDoc 为核心状态模型，通过 WebDAV、S3 和 YSync 三种协议提供访问能力的统一文件同步 Server。**

核心关系：

```text
                 One File
                    │
                   YDoc
                    │
       ┌────────────┼────────────┐
       │            │            │
     WebDAV         S3          YSync
       │            │            │
    Full I/O      Full I/O    CRDT Sync
                               │
                         ┌─────┴─────┐
                         │           │
                    Incremental     Full
```

第一版采用：

```text
Rust
+
Yrs
+
SQLite
+
Local Filesystem
```

以单节点、轻量、无外部依赖为目标。

未来通过抽象层扩展：

```text
SQLite
    ↓
PostgreSQL

Filesystem
    ↓
Object Storage

Local Room
    ↓
Distributed Room

Single Server
    ↓
Cluster
```

**MVP 的第一优先级不是规模，而是把“一文件一 YDoc + 三协议统一状态模型”完整跑通。**

