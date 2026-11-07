# MeiSync 结构化文档（ContentShape）改动方案

**状态：** 提案 / 待评审
**影响面：** `crates/core`、`crates/document`、`crates/api`、`crates/ysync`
**关联：** `docs/mvp.md` §2.2（YDoc 是唯一 Source of Truth）、§2.3（所有写操作进入 YDoc）、§6（YDoc 内容模型）、§26（ETag）、§37（一致性原则）
**不改：** WebDAV / S3 的对外协议与语义、持久化格式（snapshot + update log）、行导向文本文件（`.jsonl` / `.md` / `.txt`）的同步方式

---

## 1. 动机

### 1.1 现象

一个客户端（meimusic）把「整份值」类的 JSON 文件放进实时通道同步，出现了两个问题：

1. **手改被覆盖。** 用户在服务端手工改了 `recommendations.json`（WebDAV PUT / Web UI），客户端拉到了这份新内容，**又用自己的规则把它判成"更旧"并写了回去**；
2. **顺序写也不按时间顺序。** 两次写入在时间上明确先后（18:37 → 18:40），却由客户端比较值内部的字段（`generated_at_ms`）和设备时钟印章来裁决，而不是由"文档现在是什么"决定。

抓到的日志（脱敏）：

```text
sync: recommendations: account 30@179398974560, local 30@1793989734560, baseline recorded
  → generation Some(Local) (account Some(Unstamped), local Some(Stamp(Hlc { … })))
```

（这一例里那个时间戳是用户手工改小的，**不是**传输损坏；重点在于：文档本来已经持有"后写的那份值"，客户端又用自己的判据推翻了它。）

### 1.2 根因

- 内容模型（`docs/mvp.md` §6）只有一种：根 `Y.Text("content")`；
- 「整份替换」被实现成**文本手术**（`crates/core/src/file.rs` `replace_content()`：删光所有字符 → 插入新字符）。两个写者并发时，Yjs 忠实按**字符**合并，结果是两份值拼在一起（`{…}{…}`）或互相穿插；
- 因为文档里只剩字符，结构丢了，客户端只能在应用层把结构重新猜一遍：字段级 HLC 印章、上次收敛基线（`live-baseline.json`）、生成时间比较、已读状态与快照的代际耦合……每发现一种新形态就要补一条规则；
- 而"谁后写"始终没有权威依据：比的是**值里的字段**和设备时钟，不是**写入发生的顺序**。

---

## 2. 现状（对照代码）

| 环节 | 位置 | 行为 |
| --- | --- | --- |
| 写（WebDAV PUT / S3 PutObject / JSON PUT / COPY） | `crates/core/src/file.rs` `replace_content()` | `get_or_insert_text(CONTENT_KEY)` → `remove_range(0, len)` → `push(content)` |
| 读（GET / GetObject / HEAD / PROPFIND） | `crates/document/src/loaded.rs` `LoadedDocument::content()` | `meisync_core::read_text(&doc)` |
| 快照 | `DocumentHandle::read()` | `content = guard.bytes()`；`size = content.len()`；`etag = guard.etag` |
| ETag | `loaded.rs` `state_etag()` | 当前 YDoc 状态（`encode_state_as_update_v1`）的 SHA-256，**与内容形状无关** |
| 协议 | `crates/api/src/sync.rs` | `SYNC_PROTOCOL_VERSION = 1`，`capabilities` 列表，manifest 每条含 `etag` / `version` / `updated_at` / `content_type` |

关键点：**WebDAV / S3 / YSync 都不是独立存储，而是同一个 YDoc 的投影**（§2.2、§37 原则 5）。所以只要投影正确，协议语义不变。

---

## 3. 目标与非目标

### 目标

1. 「整份替换」是**原子**的：并发写不会拼出第三份值；
2. **顺序写按因果覆盖**（后写的一定赢）——因果序即时间顺序，不需要任何时间戳；
3. **真正并发**时：由已合入文档的一方决定，各端结果一致（"以文档为准"）；
4. WebDAV / S3 的对外语义、ETag、条件写、大小全部不变；
5. 客户端不再需要任何"谁更新"的判据。

### 非目标

- 不改行导向文件（`.jsonl` / `.md` / `.txt`）的同步方式：它们靠行合并，是文本 CRDT 的正确用法；
- 不改持久化格式（snapshot / update log / compaction）；
- 不做实体级合并（按 id 并集）——那是后续可选的 `Entities` 形状，见 §11；
- 不引入 `updated_at` 时间戳参与裁决（理由见 §4.4）。

---

## 4. 方案

### 4.1 内容形状

给文件增加一个**内容形状**，由内容类型推导，**不新增数据库列**：

```rust
// crates/core/src/file.rs
pub enum ContentShape {
    /// 根 Y.Text：人类可编辑的文本、行导向格式（.md/.txt/.jsonl）
    Text,
    /// 根 Y.Map：整份值（application/json，object 或 array 都算）
    Value,
}

pub fn content_shape(content_type: &str) -> ContentShape {
    if content_type.starts_with("application/json") { ContentShape::Value } else { ContentShape::Text }
}
```

`guess_content_type()` 已经能判断 `json` / `jsonl`，所以 `.json` 自动落到 `Value`，`.jsonl` 仍是 `Text`。顶层是 object 还是 array **不影响**选择：`Value` 存的是整份文件文本，形状无关。

### 4.2 根类型

```text
Text 形状（现状，不变）
YDoc
└── Y.Text("content")
    └── "# TODO\n…"

Value 形状（新增）
YDoc
└── Y.Map("value")
    ├── "value": <整份文件文本，规范化 JSON 字符串>
    └── (可选) "writer": <最后一次写入的来源，仅用于诊断>
```

为什么是**新的根 key** `"value"` 而不是复用 `"content"`：

- Yjs 的根类型一旦建立就不能原地改成别的类型；
- `yrs` 的 `get_or_insert_*` 遇到不同类型会**静默重新解释**（`transaction.rs` 注释：“it will be reinterpreted as a text”），不会报错。复用同名会让"用错 getter"变成一个无声的读空。

为什么是一个 `"value"` key 存整份文本，而不是每个字段一个 key：

- 我们的整份值文件（推荐快照、设置、曲库索引）都是「整份只有一个赢家」；
- 一个 key = 一个寄存器，**并发写由 CRDT 原子地选一个**；
- `seen` 这类附属状态跟快照在同一个字符串里，**天然原子**——"新的一代到达时带着自己的未读状态"这条语义自动成立，不需要客户端写规则。

### 4.3 为什么不用 `Y.Array`

`Y.Array` 的合并语义是**按位置**，对整份值文件是错的：

| 场景 | 寄存器（`Y.Map` 一个 key） | `Y.Array` |
| --- | --- | --- |
| 两端各生成一份 30 首的推荐快照 | 一份赢，30 首 | 交错成 60 首（谁都没要） |
| 已读状态跟着快照 | 天然原子（同一字符串） | 位置不同，顺序不可预测 |
| 文件 API 整份 PUT | `map.set` 一次完成 | 无法表达整份替换，需先 diff 出位置级操作 |

数组的**顺序本身是值的一部分**，两端并发插入时由 client id 排序，结果不是任何一方想要的。需要"两端新增都保留"的文件，正确形态是行导向文本（`.jsonl`）或按实体 id 的 `Y.Map` + 显式顺序字段，见 §11。

### 4.4 为什么不需要 `updated_at`

- **顺序写**：任何一次 `map.set` 的 item 都整合在"写者当时已看到的文档状态"之后，因果上晚于前一次写入 → 后者赢，各端一致。这就是时间顺序，不需要时钟。
- **真正并发**：由 CRDT 选一个（Yjs 对同一 key 的并发 `set` 按 client id 定胜负，确定且一致）。客户端**不需要知道谁赢**，只需要"以文档为准"。
- 因此 `updated_at` 既不能带来更好的语义，又会把"时间"重新引入成裁决依据（这正是当前复杂度的来源）。

如果将来需要「并发时也严格按服务器时间」，再加一个 `"updated_at"` key（服务器毫秒）即可，是纯增量改动，不影响本方案。

### 4.5 合并语义（客户端侧）

文档是真相，本地文件是缓存，`baseline` 是"上次与文档一致的值"：

```rust
let doc_value = map.get("value");          // 真相
let local     = 读本地文件;                 // 缓存
let baseline  = 上次与文档一致的值;          // 本机记录

match (local == baseline, doc_value == baseline) {
    (true,  true)  => {}                                    // 已一致
    (false, true)  => map.set("value", local),              // 只有本地变了 → 推（顺序：新于 baseline）
    (true,  false) => 写本地文件(doc_value),                 // 只有文档变了 → 拉（顺序：新于 baseline）
    (false, false) => { 备份(local); 写本地文件(doc_value) } // 双方都变了 → 文档赢（"服务器决定"）
}
```

四条分支、无时钟、无印章。顺序写走"推 / 拉"，并发写走"文档赢"，两端收敛。

---

## 5. 代码改动点

| # | 位置 | 现在 | 改成 | 漏改的后果 |
| --- | --- | --- | --- | --- |
| 1 | `document/src/loaded.rs` `LoadedDocument::content()` | `read_text(&doc)` | 按形状：`Text → read_text`；`Value → map.get("value")` | 读路径全部错（WebDAV/S3 拿到空内容） |
| 2 | `document/src/loaded.rs` `restore_document()` 的 `size` | `read_text(&doc).len()` | `content().len()` | **重启后**文件大小变 0、内容却正常（最难查的一种） |
| 3 | `document/src/loaded.rs` `empty_document()` 的 `size` | `read_text(&doc).len()` | 同上 | 新建文件的大小/元数据错 |
| 4 | `core/src/file.rs` `replace_content()` | `get_or_insert_text` + 删光 + 插入 | `Value → map.insert("value", content)`（一次事务） | 写入仍走文本手术，问题原样保留 |
| 5 | `document/src/loaded.rs` `prime_text()` | 建根 `Y.Text` | `Value → get_or_insert_map("value")` | 新文档里 `map.get` 为 `None`（可容错，但显式更好） |

另外把 `core/src/file.rs` 的 `FileDocument::materialize_string()` 改为调用 `LoadedDocument::content()`，让第 1 条成为唯一实现。

**验收要点：** 第 1–3 条必须共用同一个函数，不允许再出现第二处"从 YDoc 算内容"的代码。

---

## 6. 协议改动（`crates/api/src/sync.rs`）

1. `SYNC_PROTOCOL_VERSION: 1 → 2`（该常量的注释已写明“Bumped when a breaking change is made”）；
2. `capabilities` 增加 `"structured-docs"`；
3. manifest 的 `SyncEntry` 增加 `shape: "text" | "value"`（不影响老客户端解析 JSON）；
4. 老客户端（protocol 1）遇到 `shape == "value"` 的文件：**不建立 live doc**，退回 HTTP 同步。否则它会在 `content` 上读到空文本，并可能把空内容写回去。

`updated_at` / `version` / `etag` / `content_type` 等既有字段语义不变。

---

## 7. 迁移

**不动数据库，不做批量任务，懒迁移：**

1. 某个 `.json` 文件第一次被打开时，如果 `Y.Map("value")` 不存在、而 `Y.Text("content")` 有内容 → 在同一事务里 `map.insert("value", read_text(doc))`；
2. 老的 `content` 根保留在文档里，不再被读写（Yjs 不清理根类型；它是死数据，不影响正确性）；
3. 客户端按 manifest 的 `shape` 选 getter；protocol 1 的客户端被门在外面，继续走 HTTP；
4. **回滚**：把 `content_shape()` 改回 `Text` 即可恢复旧行为——`content` 根里仍是迁移前的原文（前提是迁移后没有再写入；如需严格回滚，可在迁移时保留一份 `sync-backup`）。

---

## 8. 对 WebDAV / S3 的影响

### 不变（明确结论）

| 项 | 原因 |
| --- | --- |
| ETag / `If-Match` / `If-None-Match` / `412` | `etag = state_etag(full_state())` 是整份文档状态的哈希，与内容形状无关；`map.set` 同样推进状态向量 |
| `version` / `sync_mode` / `content_type` / `Content-Encoding` | 都不变；`.json` 仍是 `application/json` + `ContentEncoding::Text` |
| `SyncMode::Full`（二进制 / base64） | `core/src/file.rs` 只在 base64 时才给 Full；`.json` 是 Text/Incremental，不进这条路 |
| 增量同步 / snapshot / compaction | `full_state()` / `state_vector()` / `diff()` 都是文档级的 |
| WebDAV PUT / S3 PutObject 语义 | 仍然整份替换，只是对 `Value` 形状落成一次 `map.set`；返回的 `DocumentSnapshot` 字节一致 |
| 老客户端走 HTTP | 读到的仍是同样的 JSON 字节 |

### 唯一的行为变化（也就是本方案的目的）

并发写不再把两份文本拼在一起，而是**原子地选一份**；顺序写严格按先后覆盖。

---

## 9. 客户端接入（meimusic，供对照）

1. 握手读取 `capabilities` / manifest 的 `shape`；
2. `Value` 形状：用 `Y.Map`，按 §4.5 的四分支读写；
3. `Text` 形状（`.jsonl` / `.md`）：保持现有实现；
4. 可以删除的客户端代码（实现后核对）：
   - `live/recommendations_doc.rs` 的字段级印章与全部判据（约 600 行）；
   - `Baselines.recommendations_versions`（只保留内容基线 `recommendations`）；
   - `json_body()` 对整份值的 `Doubled` 修复与"1975 年"告警；
   - `textdoc` 行锚定编辑对这三个文件的用法。

---

## 10. 测试计划

| 测试 | 覆盖 |
| --- | --- |
| `webdav_get_value_shape_json` | WebDAV GET 一个顶层为 object 和 array 的 `.json`，字节与写入一致 |
| `s3_get_value_shape_json` | S3 GetObject 同上；`Size` / `ETag` 与 WebDAV 一致 |
| `put_get_roundtrip_value_shape` | PUT → GET 往返；`Content-Length`、`HEAD`、`PROPFIND getcontentlength` 正确 |
| `restart_keeps_size_and_etag` | 写入 → 重启（从 snapshot + update log 重建）→ `size` / `etag` 不变（覆盖第 2 条） |
| `concurrent_sets_keep_one_value` | 两个 client 并发 `map.set` 不同值 → 文档只剩一份，两个 client 读到相同值 |
| `sequential_sets_last_wins` | A 写 → 同步 → B 写 → 两端都是 B 的值（时间顺序） |
| `file_api_edit_reaches_live_client` | WebDAV PUT → 已连接的 YSync 客户端在毫秒级收到（`replace_content` 经 `DocumentHandle` 广播） |
| `lazy_migration_copies_content_to_value` | 老 `Y.Text` 文档 → 首次打开后 `value` 等于原文，且 `content` 保留 |
| `protocol_1_client_falls_back_to_http` | protocol 1 客户端对 `Value` 形状不建 live doc |

---

## 11. 未决问题与后续

1. **`Entities` 形状（可选，后续）：** 需要"两端新增都保留"的文件（例如曲库索引按路径并集）可以再引入一种形状：`Y.Map` 以实体 id 为 key + 一个显式的顺序 key。这要求节点知道该文件的 schema，属于独立改动，不在本方案内。
2. **`web` / `obsidian-plugin` 客户端的 protocol 2 迁移：** 它们当前读 `Y.Text("content")`，需要按 `shape` 分支；在此之前由协议门保证它们继续走 HTTP。
3. **`updated_at` 何时需要：** 只有当"并发时也要严格按服务器时间"成为硬需求时才加（一个 `map` key），属于纯增量改动。
4. **文档同步：** 实现落地时更新 `docs/mvp.md` §6（内容模型）与 §37（一致性原则），并新增一条原则：
   > 整份值不以文本承载：`application/json` 等整份替换的语义由寄存器（`Y.Map` 的一个 key）表达，文本类型只用于行导向/人类可编辑内容。

---

## 12. 改动清单（供评审勾选）

- [ ] `crates/core/src/file.rs`：`ContentShape` + `content_shape()`；`replace_content()` 分支；`materialize_string()` 改走 `LoadedDocument::content()`
- [ ] `crates/document/src/loaded.rs`：`content()` 分支；`restore_document()` / `empty_document()` 的 `size` 统一；`prime_text()` 分支
- [ ] `crates/api/src/sync.rs`：`SYNC_PROTOCOL_VERSION = 2`；`capabilities += "structured-docs"`；`SyncEntry.shape`
- [ ] `crates/ysync`：`Value` 形状的文件建立 `Y.Map` 根；protocol 1 客户端对该类文件不建 live doc
- [ ] 懒迁移（`content` → `value`）
- [ ] 测试（§10）
- [ ] 文档：`docs/mvp.md` §6 / §37
