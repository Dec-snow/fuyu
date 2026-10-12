//! 本地 SQLite：会话与消息历史的唯一权威存储。
//!
//! 设计要点（对照参考项目做了简化，但机制一致）：
//! - **迁移用 `PRAGMA user_version` 驱动**，每一步包在事务里，失败即回滚；
//! - “加列”这一步做成**幂等**的（先查 `table_info` 再决定加不加），
//!   而不是靠匹配 `duplicate column name` 这句英文错误串；
//! - 中文搜索用 **FTS5 trigram 分词器**：无需任何分词库/向量模型，
//!   对中文按三字符滑窗建索引，`MATCH` 即子串匹配；
//! - 打开 WAL 并设置 `busy_timeout`，为将来多连接留出余地。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension};

use crate::errors::{AppError, AppResult};
use crate::kb::{self, KbDocument, KbHit, KbStats};
use crate::models::chat::{Conversation, MessageHit, MessageStatus, MessageText, StoredMessage};
use crate::storage::backup::{Backup, BackupConversation, BackupMessage, ImportReport};
use crate::storage::{new_id, now_ms};

/// 一步迁移。
enum Step {
    /// 一段可直接执行的 SQL。
    Sql(&'static str),
    /// 幂等加列：列已存在就跳过。
    ///
    /// 迁移脚本在“库已经是新结构、但 `user_version` 落后”的场景下会重复
    /// 执行（例如手工改过库、或从中断的升级里恢复），此时 `ALTER TABLE`
    /// 会抛 `duplicate column name`。与其去匹配这句英文错误串（脆弱，
    /// SQLite 换个语言/版本就失效），不如先查一下列是否存在。
    AddColumn {
        table: &'static str,
        column: &'static str,
        decl: &'static str,
    },
}

/// 迁移表：下标 + 1 就是这一版的目标 `user_version`。
///
/// **只允许往后追加，永远不要修改或删除已发布的条目** —— 用户机器上的库
/// 已经按旧条目升过级了。
const MIGRATIONS: &[Step] = &[
    // v1 —— 会话与消息
    Step::Sql(
        "CREATE TABLE IF NOT EXISTS conversations (
            id            TEXT PRIMARY KEY,
            title         TEXT NOT NULL DEFAULT '新对话',
            provider_id   TEXT NOT NULL DEFAULT '',
            model_key     TEXT NOT NULL DEFAULT '',
            system_prompt TEXT,
            pinned        INTEGER NOT NULL DEFAULT 0,
            created_at    INTEGER NOT NULL,
            updated_at    INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS messages (
            id              TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            role            TEXT NOT NULL,
            content         TEXT NOT NULL,
            provider_id     TEXT,
            model_key       TEXT,
            created_at      INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_messages_conversation
            ON messages(conversation_id, created_at);
        CREATE INDEX IF NOT EXISTS idx_conversations_updated
            ON conversations(pinned DESC, updated_at DESC);",
    ),
    // v2 —— 消息全文检索（FTS5，trigram 分词器）
    //
    // `content='messages'` 是「外部内容表」：FTS 表本身不存正文，只存索引，
    // 正文永远以 messages 为准，不会出现两份数据不同步。
    // 代价是删除/更新必须显式走 'delete' 指令 —— 见下面三个触发器。
    Step::Sql(
        "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
            content,
            content='messages',
            content_rowid='rowid',
            tokenize='trigram'
        );
        CREATE TRIGGER IF NOT EXISTS messages_fts_ai AFTER INSERT ON messages BEGIN
            INSERT INTO messages_fts(rowid, content) VALUES (new.rowid, new.content);
        END;
        CREATE TRIGGER IF NOT EXISTS messages_fts_ad AFTER DELETE ON messages BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, content)
            VALUES ('delete', old.rowid, old.content);
        END;
        CREATE TRIGGER IF NOT EXISTS messages_fts_au AFTER UPDATE ON messages BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, content)
            VALUES ('delete', old.rowid, old.content);
            INSERT INTO messages_fts(rowid, content) VALUES (new.rowid, new.content);
        END;",
    ),
    // v3 —— 思考过程与 token 用量
    Step::AddColumn {
        table: "messages",
        column: "reasoning",
        decl: "TEXT",
    },
    Step::AddColumn {
        table: "messages",
        column: "input_tokens",
        decl: "INTEGER",
    },
    Step::AddColumn {
        table: "messages",
        column: "output_tokens",
        decl: "INTEGER",
    },
    // v4 —— 会话级默认系统提示词与最后使用的模型已在 v1 建好，
    //        这一版补一个“是否置顶”的排序索引与消息软标记。
    Step::Sql(
        "CREATE INDEX IF NOT EXISTS idx_messages_role
            ON messages(conversation_id, role);",
    ),
    // v7 —— 消息终态：失败 / 取消也要落库，否则那条失败的对话下次打开就
    //        凭空消失了，既看不到原因，也没有「重试」的锚点。
    Step::AddColumn {
        table: "messages",
        column: "status",
        decl: "TEXT NOT NULL DEFAULT 'ok'",
    },
    // v8 —— 失败详情（`AppError` 的 JSON），仅当 status = 'error' 时有值。
    Step::AddColumn {
        table: "messages",
        column: "error_json",
        decl: "TEXT",
    },
    // v9 —— 会话的滚动摘要。
    //
    // 上下文超预算时，与其把最早的历史直接**丢掉**（模型是真的「忘了」），
    // 不如先把它压缩成一段摘要留在提示词里。
    Step::AddColumn {
        table: "conversations",
        column: "summary",
        decl: "TEXT NOT NULL DEFAULT ''",
    },
    // v10 —— 摘要覆盖到了第几条消息（按 `created_at` 升序计数）。
    //         下一轮只把这条之后的原文发给模型，前面的用摘要代替。
    Step::AddColumn {
        table: "conversations",
        column: "summary_message_count",
        decl: "INTEGER NOT NULL DEFAULT 0",
    },
    // v11 —— 用户消息携带的图片（data URL 的 JSON 数组）。
    //
    // 为什么不把图片散落到磁盘上：存进同一个库，**删会话即删图**可以完全交给
    // 外键级联，不会留下孤儿文件，也不用做引用计数。代价是库文件会变大 ——
    // 单条消息的图片数量与体积由 `save_user_message` 把关（4 张 / 每张约 2 MB）。
    Step::AddColumn {
        table: "messages",
        column: "images_json",
        decl: "TEXT",
    },
    // v12 —— 摘要覆盖到的**最后一条消息 id**。
    //
    // 为什么不能用 `summary_message_count`（已覆盖条数）去切数组：那个计数是
    // **数据库行数**，而要切的是**前端发来的那个数组** —— 两者并不等长：
    // - 前端会过滤掉「正文为空且没有图片」的消息（历史里的失败轮就是这种）；
    // - 用户「编辑重发」会把某条之后的消息**删掉**，行数立刻变少，
    //   而覆盖条数还停在旧值上。
    // 位置一旦错位，`messages[covered..]` 就可能把**用户刚问的那句话**当成
    // 「已被摘要覆盖」而整条丢掉 —— 请求发出去只剩摘要，没有提问。
    //
    // 所以改成按 id 定位：找不到那条就**整段放弃摘要**（宁可多发点历史，
    // 也绝不能丢用户的提问）。`summary_message_count` 只保留给界面展示。
    Step::AddColumn {
        table: "conversations",
        column: "summary_covered_message_id",
        decl: "TEXT",
    },
    // v13 —— 这个会话用哪个人设（角色预设的 id）。
    //
    // 为什么存 id 而不是把人设的提示词抄进 `system_prompt`：
    // - 抄一份的话，用户改了角色的措辞，已有会话不会跟着变（像「改了没用」）；
    // - 界面没法显示「当前是哪个角色」，只能拿提示词去反查，改个标点就匹配不上。
    //
    // 可空：NULL 表示没有指定人设，此时回落到全局系统提示词。
    Step::AddColumn {
        table: "conversations",
        column: "role_id",
        decl: "TEXT",
    },
    // v14 —— 本地知识库（BM25 倒排索引）。
    //
    // 为什么不用 FTS5：消息搜索用的是 trigram 分词器（为了中文子串匹配），
    // 而 trigram 下 `bm25()` 几乎没有区分度（三元组遍地都是）；真正的中文 BM25
    // 需要「词」的概念，FTS5 的 unicode61 又不切中文。自己维护倒排表反而更可控，
    // 而且分词与打分都是纯函数、能单测。
    //
    // 四张表的分工：
    // - `kb_documents`：文档元信息（列表用）
    // - `kb_chunks`：切片正文 + token 数（BM25 的长度归一化要用）
    // - `kb_postings`：(词, 片段) → 词频。`WITHOUT ROWID` 是因为它就是个
    //   复合主键的映射表，不需要额外的 rowid
    // - `kb_terms`：词 → 文档频率（df）。单独存一份是为了查 df 时不用扫倒排表；
    //   它在收录/删除时同步维护，删除时要**重算**（见 `kb_delete_document`）
    Step::Sql(
        "CREATE TABLE IF NOT EXISTS kb_documents (
            id          TEXT PRIMARY KEY,
            title       TEXT NOT NULL,
            source      TEXT NOT NULL DEFAULT '',
            chars       INTEGER NOT NULL DEFAULT 0,
            chunk_count INTEGER NOT NULL DEFAULT 0,
            added_at    INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS kb_chunks (
            id          TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES kb_documents(id) ON DELETE CASCADE,
            ordinal     INTEGER NOT NULL,
            text        TEXT NOT NULL,
            tokens      INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS kb_chunks_document ON kb_chunks(document_id);
        CREATE TABLE IF NOT EXISTS kb_postings (
            term     TEXT NOT NULL,
            chunk_id TEXT NOT NULL REFERENCES kb_chunks(id) ON DELETE CASCADE,
            tf       INTEGER NOT NULL,
            PRIMARY KEY (term, chunk_id)
        ) WITHOUT ROWID;
        CREATE TABLE IF NOT EXISTS kb_terms (
            term TEXT PRIMARY KEY,
            df   INTEGER NOT NULL
        ) WITHOUT ROWID;",
    ),
];

/// 当前代码期望的库版本。
pub fn target_version() -> i64 {
    MIGRATIONS.len() as i64
}

/// 打开会话时，最多为最近多少条消息加载图片。
///
/// 界面上够用（用户基本只看最近的），同时把一次会话加载的图片量封在上界内：
/// 20 条 × 4 张 × 3 MB ≈ 最多 240 MB 文本 —— 仍然不小，但比「整个会话不限量」
/// 的几百 MB～1 GB 好得多，而且不会随会话变长继续恶化。
pub const IMAGE_HISTORY_LIMIT: i64 = 20;

/// 新建会话的默认标题。
///
/// 显示层（自动命名判断）与 SQL 默认值共用同一个常量，避免两处各写一份
/// 中文字面量、改一处忘一处。
pub const DEFAULT_CONVERSATION_TITLE: &str = "新对话";

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    path: Arc<PathBuf>,
}

impl Database {
    /// 打开（或新建）数据库，跑完迁移后返回。
    pub fn open(path: &Path) -> AppResult<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;

        // 注意：`journal_mode` 这类 PRAGMA 会返回一行结果，必须用
        // `execute_batch`（它忽略返回行），不能用 `execute`。
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;",
        )?;

        migrate(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: Arc::new(path.to_path_buf()),
        })
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> AppResult<T>) -> AppResult<T> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| AppError::db("数据库连接锁已损坏"))?;
        f(&guard)
    }

    // ---------------------------------------------------------------- 会话

    pub fn list_conversations(&self, limit: i64) -> AppResult<Vec<Conversation>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT c.id, c.title, c.provider_id, c.model_key, c.system_prompt,
                        c.pinned, c.created_at, c.updated_at,
                        (SELECT COUNT(*) FROM messages m WHERE m.conversation_id = c.id),
                        c.summary, c.summary_message_count, c.summary_covered_message_id,
                        c.role_id
                 FROM conversations c
                 ORDER BY c.pinned DESC, c.updated_at DESC
                 LIMIT ?1",
            )?;
            let rows = stmt.query_map([limit], map_conversation)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn get_conversation(&self, id: &str) -> AppResult<Option<Conversation>> {
        self.with_conn(|conn| {
            let conv = conn
                .query_row(
                    "SELECT c.id, c.title, c.provider_id, c.model_key, c.system_prompt,
                            c.pinned, c.created_at, c.updated_at,
                            (SELECT COUNT(*) FROM messages m WHERE m.conversation_id = c.id),
                            c.summary, c.summary_message_count, c.summary_covered_message_id,
                            c.role_id
                     FROM conversations c WHERE c.id = ?1",
                    [id],
                    map_conversation,
                )
                .optional()?;
            Ok(conv)
        })
    }

    /// 写入滚动摘要。
    ///
    /// - `covered` 是摘要已覆盖的消息**条数**（只用于界面展示「较早的 N 条已压缩」）；
    /// - `covered_message_id` 是覆盖到的**最后一条消息 id**（真正用来切上下文）。
    ///
    /// 只更新摘要这几列，**不动** `updated_at` —— 摘要是后台压缩产生的，
    /// 不该让一个旧会话因此跳到会话列表最前面。
    pub fn set_conversation_summary(
        &self,
        conversation_id: &str,
        summary: &str,
        covered: i64,
        covered_message_id: Option<&str>,
    ) -> AppResult<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE conversations
                    SET summary = ?2,
                        summary_message_count = ?3,
                        summary_covered_message_id = ?4
                  WHERE id = ?1",
                rusqlite::params![conversation_id, summary, covered, covered_message_id],
            )?;
            Ok(())
        })
    }

    /// 读一个会话的全部消息（按时间升序）。
    ///
    /// **只有最近 [`IMAGE_HISTORY_LIMIT`] 条消息会带图片**。图片是原样的
    /// base64（单条最多 4 张 × 3 MB），一个截图很多的会话能轻松累积到几百 MB；
    /// 每次打开会话都把整段读出来、解析成 `Vec<String>`、再经 IPC 序列化一遍，
    /// 会让切会话明显卡顿甚至爆内存。
    ///
    /// 代价：很早以前的图片在**界面上**不显示了。模型那边不受影响 ——
    /// 发请求时 `attach_images` 是按 id 直接从库里取的，不经过这条路径。
    pub fn list_messages(&self, conversation_id: &str) -> AppResult<Vec<StoredMessage>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT m.id, m.conversation_id, m.role, m.content, m.reasoning, m.provider_id,
                        m.model_key, m.input_tokens, m.output_tokens, m.created_at,
                        m.status, m.error_json,
                        CASE WHEN m.rowid >= COALESCE(
                               (SELECT MIN(t.rowid) FROM (
                                  SELECT rowid FROM messages
                                   WHERE conversation_id = ?1
                                   ORDER BY created_at DESC, rowid DESC
                                   LIMIT ?2) AS t), 0)
                             THEN m.images_json ELSE NULL END
                   FROM messages m
                  WHERE m.conversation_id = ?1
                  ORDER BY m.created_at ASC, m.rowid ASC",
            )?;
            let rows = stmt.query_map(
                rusqlite::params![conversation_id, IMAGE_HISTORY_LIMIT],
                map_message,
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn create_conversation(
        &self,
        provider_id: &str,
        model_key: &str,
        title: Option<&str>,
    ) -> AppResult<Conversation> {
        let id = new_id("conv_");
        let now = now_ms();
        let title = title.unwrap_or(DEFAULT_CONVERSATION_TITLE).to_string();
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO conversations
                   (id, title, provider_id, model_key, system_prompt, pinned, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, NULL, 0, ?5, ?5)",
                rusqlite::params![id, title, provider_id, model_key, now],
            )?;
            Ok(Conversation {
                id,
                title,
                provider_id: provider_id.to_string(),
                model_key: model_key.to_string(),
                system_prompt: None,
                pinned: false,
                created_at: now,
                updated_at: now,
                message_count: 0,
                // 新会话还没有摘要；上面 INSERT 用的是列默认值。
                summary: String::new(),
                summary_message_count: 0,
                summary_covered_message_id: None,
                role_id: None,
            })
        })
    }

    /// 设置会话使用的人设（`None` = 回落到全局系统提示词）。
    ///
    /// 与 `set_conversation_pinned` 一样**不动** `updated_at`：
    /// 换个角色不是「有新内容」，不该让这个会话跳到列表最前面。
    pub fn set_conversation_role(&self, id: &str, role_id: Option<&str>) -> AppResult<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE conversations SET role_id = ?2 WHERE id = ?1",
                rusqlite::params![id, role_id],
            )?;
            Ok(())
        })
    }

    pub fn rename_conversation(&self, id: &str, title: &str) -> AppResult<()> {
        let title = title.trim();
        let title = if title.is_empty() { "新对话" } else { title };
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE conversations SET title = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id, title, now_ms()],
            )?;
            Ok(())
        })
    }

    pub fn set_conversation_pinned(&self, id: &str, pinned: bool) -> AppResult<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE conversations SET pinned = ?2 WHERE id = ?1",
                rusqlite::params![id, pinned as i64],
            )?;
            Ok(())
        })
    }

    /// 删除会话。`messages` 上有 `ON DELETE CASCADE`，所以消息与 FTS 索引
    /// 会一起被清掉（FTS 靠 DELETE 触发器同步）。
    pub fn delete_conversation(&self, id: &str) -> AppResult<()> {
        self.with_conn(|conn| {
            conn.execute("DELETE FROM conversations WHERE id = ?1", [id])?;
            Ok(())
        })
    }

    pub fn clear_all_history(&self) -> AppResult<()> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute("DELETE FROM messages", [])?;
            tx.execute("DELETE FROM conversations", [])?;
            tx.commit()?;
            Ok(())
        })
    }

    /// 当前库结构版本（诊断页要显示它：升级出问题时这是第一个要看的值）。
    pub fn schema_version(&self) -> AppResult<i64> {
        self.with_conn(|conn| Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?))
    }

    /// `(会话数, 消息数)`。诊断页用。
    pub fn counts(&self) -> AppResult<(i64, i64)> {
        self.with_conn(|conn| {
            let conversations: i64 =
                conn.query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))?;
            let messages: i64 =
                conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
            Ok((conversations, messages))
        })
    }

    // ------------------------------------------------------------------
    // 本地知识库（BM25）
    // ------------------------------------------------------------------

    /// 收录一篇文档：切块 + 建倒排索引，**全在一个事务里**。
    ///
    /// 不整篇重算：只在 `kb_terms` 上把命中的词的 df 各加一。这样收录一篇
    /// 大文档的代价与文档本身成正比，而不是与整个库成正比。
    pub fn kb_add_document(&self, title: &str, source: &str, text: &str) -> AppResult<KbDocument> {
        let title = title.trim();
        let text = text.trim();
        if title.is_empty() {
            return Err(AppError::invalid("文档标题不能为空"));
        }
        if text.is_empty() {
            return Err(AppError::invalid("文档内容为空，没有可索引的文字"));
        }
        let chars = text.chars().count();
        if chars > kb::MAX_DOCUMENT_CHARS {
            return Err(AppError::invalid(format!(
                "文档太长（{chars} 字，上限 {} 字）",
                kb::MAX_DOCUMENT_CHARS
            )));
        }

        let chunks = kb::chunk_text(text, kb::DEFAULT_CHUNK_CHARS, kb::DEFAULT_OVERLAP_CHARS);
        if chunks.is_empty() {
            return Err(AppError::invalid("文档内容为空，没有可索引的文字"));
        }

        let document_id = new_id("kbd_");
        let now = now_ms();
        let chunk_count = chunks.len() as i64;

        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO kb_documents (id, title, source, chars, chunk_count, added_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![document_id, title, source, chars as i64, chunk_count, now],
            )?;

            for (index, chunk) in chunks.iter().enumerate() {
                let chunk_id = new_id("kbc_");
                let tokens = kb::tokenize(chunk);
                // 词频表：同一片里出现几次就是几次；顺带算出去重后的词表
                let mut tf: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
                for token in &tokens {
                    *tf.entry(token.as_str()).or_insert(0) += 1;
                }

                tx.execute(
                    "INSERT INTO kb_chunks (id, document_id, ordinal, text, tokens)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        chunk_id,
                        document_id,
                        index as i64,
                        chunk,
                        tokens.len().min(kb::MAX_TOKENS_PER_CHUNK) as i64
                    ],
                )?;

                for (term, count) in &tf {
                    tx.execute(
                        "INSERT INTO kb_postings (term, chunk_id, tf) VALUES (?1, ?2, ?3)",
                        rusqlite::params![term, chunk_id, count],
                    )?;
                    tx.execute(
                        "INSERT INTO kb_terms (term, df) VALUES (?1, 1)
                         ON CONFLICT(term) DO UPDATE SET df = df + 1",
                        rusqlite::params![term],
                    )?;
                }
            }

            tx.commit()?;
            Ok(KbDocument {
                id: document_id.clone(),
                title: title.to_string(),
                source: source.to_string(),
                chars: chars as i64,
                chunk_count,
                added_at: now,
            })
        })
    }

    pub fn kb_list_documents(&self) -> AppResult<Vec<KbDocument>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, title, source, chars, chunk_count, added_at
                   FROM kb_documents
                  ORDER BY added_at DESC, id ASC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(KbDocument {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    source: row.get(2)?,
                    chars: row.get(3)?,
                    chunk_count: row.get(4)?,
                    added_at: row.get(5)?,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// 删除一篇文档。切片与倒排项靠外键级联删掉，**df 必须自己重算**。
    ///
    /// 为什么不能直接 `df -= 1`：一个词可能在一篇文档的多个片段里都出现，
    /// 删掉这篇文档后 df 要减少的量是「这篇文档里包含该词的片段数」——
    /// 逐个算容易错，直接按剩余的倒排项重数一遍最稳（删除本来就不频繁）。
    /// 另外词可能只存在于被删的文档里，此时 df 归零、必须把词也删掉，
    /// 否则 `idf` 会基于一个不存在的词计算，搜索时会凭空给分。
    pub fn kb_delete_document(&self, id: &str) -> AppResult<()> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;

            // 先记下这篇文档用到了哪些词（删完就查不到了）
            let mut terms: Vec<String> = Vec::new();
            {
                let mut stmt = tx.prepare(
                    "SELECT DISTINCT p.term FROM kb_postings p
                       JOIN kb_chunks c ON c.id = p.chunk_id
                      WHERE c.document_id = ?1",
                )?;
                let rows = stmt.query_map([id], |row| row.get::<_, String>(0))?;
                for row in rows {
                    terms.push(row?);
                }
            }

            tx.execute("DELETE FROM kb_documents WHERE id = ?1", [id])?;

            for term in &terms {
                let remaining: i64 = tx.query_row(
                    "SELECT COUNT(DISTINCT chunk_id) FROM kb_postings WHERE term = ?1",
                    [term],
                    |row| row.get(0),
                )?;
                if remaining <= 0 {
                    tx.execute("DELETE FROM kb_terms WHERE term = ?1", [term])?;
                } else {
                    tx.execute(
                        "UPDATE kb_terms SET df = ?2 WHERE term = ?1",
                        rusqlite::params![term, remaining],
                    )?;
                }
            }

            tx.commit()?;
            Ok(())
        })
    }

    pub fn kb_stats(&self) -> AppResult<KbStats> {
        self.with_conn(|conn| {
            let documents: i64 =
                conn.query_row("SELECT COUNT(*) FROM kb_documents", [], |r| r.get(0))?;
            let chunks: i64 = conn.query_row("SELECT COUNT(*) FROM kb_chunks", [], |r| r.get(0))?;
            let terms: i64 = conn.query_row("SELECT COUNT(*) FROM kb_terms", [], |r| r.get(0))?;
            let chars: i64 = conn.query_row(
                "SELECT COALESCE(SUM(chars), 0) FROM kb_documents",
                [],
                |r| r.get(0),
            )?;
            Ok(KbStats {
                documents,
                chunks,
                terms,
                chars,
            })
        })
    }

    /// 库里有多少篇文档。工具清单用它决定要不要下发检索工具。
    pub fn kb_document_count(&self) -> AppResult<i64> {
        self.with_conn(|conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM kb_documents", [], |r| r.get(0))?)
        })
    }

    /// BM25 检索。
    ///
    /// 分三步：拿到候选倒排项（一条 JOIN 查询）→ 纯函数打分排序 →
    /// 取前 N 片的正文。第三步只对最终结果查一次，不去读全部片段。
    pub fn kb_search(&self, query: &str, limit: usize) -> AppResult<Vec<KbHit>> {
        let terms = kb::query_terms(query);
        let limit = limit.clamp(1, kb::MAX_SEARCH_LIMIT);
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        self.with_conn(|conn| {
            let total_chunks: i64 =
                conn.query_row("SELECT COUNT(*) FROM kb_chunks", [], |r| r.get(0))?;
            if total_chunks == 0 {
                return Ok(Vec::new());
            }

            let placeholders = vec!["?"; terms.len()].join(",");
            let params: Vec<&dyn rusqlite::ToSql> =
                terms.iter().map(|t| t as &dyn rusqlite::ToSql).collect();

            // ① 倒排项（带片段长度，BM25 的长度归一化要用）
            let postings_sql = format!(
                "SELECT p.term, p.chunk_id, p.tf, c.tokens
                   FROM kb_postings p JOIN kb_chunks c ON c.id = p.chunk_id
                  WHERE p.term IN ({placeholders})"
            );
            let mut postings: HashMap<String, Vec<kb::rank::Posting>> = HashMap::new();
            {
                let mut stmt = conn.prepare(&postings_sql)?;
                let rows = stmt.query_map(params.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?;
                for row in rows {
                    let (term, chunk_id, tf, tokens) = row?;
                    postings
                        .entry(term)
                        .or_default()
                        .push((chunk_id, tf, tokens));
                }
            }
            if postings.is_empty() {
                return Ok(Vec::new());
            }

            // ② 文档频率
            let df_sql = format!("SELECT term, df FROM kb_terms WHERE term IN ({placeholders})");
            let mut doc_freq: HashMap<String, i64> = HashMap::new();
            {
                let mut stmt = conn.prepare(&df_sql)?;
                let rows = stmt.query_map(params.as_slice(), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?;
                for row in rows {
                    let (term, df) = row?;
                    doc_freq.insert(term, df);
                }
            }

            // ③ 打分（纯函数）
            let ranked = kb::rank(&postings, &doc_freq, total_chunks, limit);
            if ranked.is_empty() {
                return Ok(Vec::new());
            }

            // ④ 取正文：只查最终这几片的正文与所属文档
            let id_placeholders = vec!["?"; ranked.len()].join(",");
            let detail_sql = format!(
                "SELECT c.id, c.document_id, c.ordinal, c.text, d.title
                   FROM kb_chunks c JOIN kb_documents d ON d.id = c.document_id
                  WHERE c.id IN ({id_placeholders})"
            );
            let id_params: Vec<&dyn rusqlite::ToSql> = ranked
                .iter()
                .map(|s| &s.chunk_id as &dyn rusqlite::ToSql)
                .collect();

            let mut details: HashMap<String, (String, i64, String, String)> = HashMap::new();
            {
                let mut stmt = conn.prepare(&detail_sql)?;
                let rows = stmt.query_map(id_params.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?;
                for row in rows {
                    let (chunk_id, document_id, ordinal, text, title) = row?;
                    details.insert(chunk_id, (document_id, ordinal, text, title));
                }
            }

            // 按打分顺序回填（HashMap 会打乱顺序，这里必须用 ranked 的次序）
            let mut hits = Vec::with_capacity(ranked.len());
            for scored in ranked {
                let Some((document_id, ordinal, text, title)) = details.remove(&scored.chunk_id)
                else {
                    continue;
                };
                hits.push(KbHit {
                    chunk_id: scored.chunk_id,
                    document_id,
                    title,
                    // 界面显示第几片：从 1 开始更自然
                    ordinal: ordinal + 1,
                    text,
                    score: scored.score,
                });
            }
            Ok(hits)
        })
    }

    /// 按「天 + 服务商 + 模型」聚合 token 用量，供费用估算与趋势图使用。
    /// 只统计**真的报过 usage 的助手消息**（两个 token 字段加起来大于 0）：
    /// - 用户消息没有 token，混进来会造出一堆「模型名为空」的行，
    ///   然后被费用估算当成「认不出来的模型」列出来 —— 那是纯噪音；
    /// - 被取消 / 失败的轮次通常没有 usage，它们也不该算钱。
    ///
    /// 日期按**本地时间**分组：用户看的是「我昨天花了多少」，不是 UTC 的昨天。
    pub fn usage_by_day_model(
        &self,
        since_ms: i64,
    ) -> AppResult<Vec<crate::models::pricing::UsageRow>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT date(created_at / 1000, 'unixepoch', 'localtime') AS day,
                        COALESCE(provider_id, '') AS provider_id,
                        COALESCE(model_key, '') AS model_key,
                        SUM(COALESCE(input_tokens, 0)) AS input_tokens,
                        SUM(COALESCE(output_tokens, 0)) AS output_tokens,
                        COUNT(*) AS messages
                   FROM messages
                  WHERE role = 'assistant'
                    AND created_at >= ?1
                    AND (COALESCE(input_tokens, 0) + COALESCE(output_tokens, 0)) > 0
                  GROUP BY day, provider_id, model_key
                  ORDER BY day ASC",
            )?;
            let rows = stmt.query_map([since_ms], |row| {
                Ok(crate::models::pricing::UsageRow {
                    day: row.get(0)?,
                    provider_id: row.get(1)?,
                    model_key: row.get(2)?,
                    input_tokens: row.get(3)?,
                    output_tokens: row.get(4)?,
                    messages: row.get(5)?,
                })
            })?;

            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
    }

    /// 导出全部会话与消息（**含图片**）。
    ///
    /// 图片原样带走：备份悄悄丢掉一部分数据比文件大更糟 ——
    /// 用户要的是「换台机器还能接着用」，而不是「一份看起来完整的残缺副本」。
    /// 代价是带大量图片时文件会很大，这一点在界面上会如实告诉用户。
    ///
    /// 这里**不走** `list_messages`：那个为了省内存只给最近若干条消息加载图片
    /// （见 `IMAGE_HISTORY_LIMIT`），导出的必须是全部。
    pub fn export_backup(&self) -> AppResult<Backup> {
        let conversations = self.list_conversations(i64::MAX)?;
        self.with_conn(|conn| {
            let mut out = Vec::with_capacity(conversations.len());
            let mut stmt = conn.prepare(
                "SELECT id, role, content, reasoning, status, error_json,
                        input_tokens, output_tokens, images_json, created_at
                   FROM messages
                  WHERE conversation_id = ?1
                  ORDER BY created_at ASC, rowid ASC",
            )?;

            for conv in conversations {
                let rows = stmt.query_map([&conv.id], |row| {
                    Ok(BackupMessage {
                        id: row.get(0)?,
                        role: row.get(1)?,
                        content: row.get(2)?,
                        reasoning: row.get(3)?,
                        status: row.get(4)?,
                        error_json: row.get(5)?,
                        input_tokens: row.get(6)?,
                        output_tokens: row.get(7)?,
                        images_json: row.get(8)?,
                        created_at: row.get(9)?,
                    })
                })?;

                let mut messages = Vec::new();
                for m in rows {
                    messages.push(m?);
                }

                out.push(BackupConversation {
                    id: conv.id,
                    title: conv.title,
                    provider_id: conv.provider_id,
                    model_key: conv.model_key,
                    role_id: conv.role_id,
                    pinned: conv.pinned,
                    created_at: conv.created_at,
                    updated_at: conv.updated_at,
                    summary: conv.summary,
                    messages,
                });
            }

            Ok(Backup {
                app: crate::storage::backup::BACKUP_APP.to_string(),
                version: crate::storage::backup::BACKUP_VERSION,
                exported_at: now_ms(),
                conversations: out,
            })
        })
    }

    /// 导入备份。**只新增，绝不覆盖**：id 已存在的会话整个跳过。
    ///
    /// 为什么不提供「合并 / 覆盖」：导入最常见的场景是「换机器 / 重装后恢复」，
    /// 那时候用户手上的库是空的；而在非空库上覆盖，一次手滑就能毁掉正在用的历史。
    /// 想真的重置，用户有「清空全部数据」这个显式入口。
    ///
    /// 整份导入在一个事务里：中途失败就一条都不落，不会留下半份数据让用户
    /// 反复重试还越导越乱。
    pub fn import_backup(&self, backup: &Backup) -> AppResult<ImportReport> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            let mut report = ImportReport::default();

            for conv in &backup.conversations {
                let exists: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM conversations WHERE id = ?1",
                    [&conv.id],
                    |row| row.get(0),
                )?;
                if exists > 0 {
                    report.skipped += 1;
                    continue;
                }

                // 标题为空会让会话在列表里变成一个看不见名字的条目 —— 补一个默认值。
                let title = if conv.title.trim().is_empty() {
                    DEFAULT_CONVERSATION_TITLE
                } else {
                    conv.title.as_str()
                };

                tx.execute(
                    "INSERT INTO conversations
                       (id, title, provider_id, model_key, role_id, pinned,
                        created_at, updated_at, summary)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    rusqlite::params![
                        conv.id,
                        title,
                        conv.provider_id,
                        conv.model_key,
                        conv.role_id,
                        conv.pinned as i64,
                        conv.created_at,
                        conv.updated_at,
                        conv.summary
                    ],
                )?;

                for m in &conv.messages {
                    tx.execute(
                        "INSERT INTO messages
                           (id, conversation_id, role, content, images_json, reasoning,
                            provider_id, model_key, input_tokens, output_tokens,
                            status, error_json, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                        rusqlite::params![
                            m.id,
                            conv.id,
                            m.role,
                            m.content,
                            m.images_json,
                            m.reasoning,
                            conv.provider_id,
                            conv.model_key,
                            m.input_tokens,
                            m.output_tokens,
                            m.status,
                            m.error_json,
                            m.created_at
                        ],
                    )?;
                    report.messages += 1;
                }
                report.imported += 1;
            }

            tx.commit()?;
            Ok(report)
        })
    }

    /// 只顶 `updated_at`，不动模型字段。
    ///
    /// 用户消息落库时用它：那时还不知道这一轮最终会用哪个模型，若顺手写入
    /// 空字符串反而会把会话记住的模型冲掉。
    ///
    /// 助手消息走 [`Database::append_assistant_turn`] —— 那边把「插消息」和
    /// 「顶时间」放在同一个事务里。
    pub fn touch_conversation_only(&self, id: &str) -> AppResult<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE conversations SET updated_at = ?2 WHERE id = ?1",
                rusqlite::params![id, now_ms()],
            )?;
            Ok(())
        })
    }

    // ---------------------------------------------------------------- 消息

    /// 落一条消息。返回新消息的 id。
    #[allow(clippy::too_many_arguments)]
    /// 插一条消息。`images` 是 data URL 列表（只有用户消息会用到）。
    ///
    /// 参数确实偏多，但每一个都对应一列；硬凑成一个结构体只会让调用方
    /// 多写一层包装。无图时传 `&[]` 即可。
    #[allow(clippy::too_many_arguments)]
    pub fn insert_message(
        &self,
        conversation_id: &str,
        role: &str,
        content: &str,
        images: &[String],
        reasoning: Option<&str>,
        provider_id: Option<&str>,
        model_key: Option<&str>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
    ) -> AppResult<String> {
        let id = new_id("msg_");
        let now = now_ms();
        let images_json = encode_images(images)?;
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO messages
                   (id, conversation_id, role, content, images_json, reasoning,
                    provider_id, model_key, input_tokens, output_tokens, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    id,
                    conversation_id,
                    role,
                    content,
                    images_json,
                    reasoning,
                    provider_id,
                    model_key,
                    input_tokens,
                    output_tokens,
                    now
                ],
            )?;
            Ok(id)
        })
    }

    /// 轻量地取回会话里的消息序列，**不读图片**，供滚动摘要使用。
    ///
    /// 为什么不复用 [`Self::list_messages`]：那个会把 `images_json`
    /// （原始 base64，单张可达数 MB）一起读出来再反序列化，而摘要只需要文字。
    /// 一个带图的长会话每轮都白读几十 MB —— 这是纯粹的浪费，且随会话变长而恶化。
    pub fn message_texts(&self, conversation_id: &str) -> AppResult<Vec<MessageText>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, role, content, (images_json IS NOT NULL)
                   FROM messages
                  WHERE conversation_id = ?1
                  ORDER BY created_at ASC, rowid ASC",
            )?;
            let rows = stmt.query_map([conversation_id], |row| {
                Ok(MessageText {
                    id: row.get(0)?,
                    role: row.get(1)?,
                    content: row.get(2)?,
                    has_images: row.get::<_, i64>(3)? != 0,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// 按消息 id 批量取图片（`id → data URL 列表`）。
    ///
    /// 前端回传历史时**不带图片字节**，只带 id；这里一次性把它们捞出来，
    /// 避免每条消息一次查询（N+1）。
    pub fn images_for_ids(&self, ids: &[String]) -> AppResult<HashMap<String, Vec<String>>> {
        let mut out = HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }

        self.with_conn(|conn| {
            // 手写占位符：rusqlite 的 `params_from_iter` 需要手动拼 `?1, ?2, …`。
            let placeholders = (1..=ids.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT id, images_json FROM messages
                  WHERE id IN ({placeholders}) AND images_json IS NOT NULL"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |row| {
                let id: String = row.get(0)?;
                let raw: Option<String> = row.get(1)?;
                Ok((id, raw))
            })?;
            for row in rows {
                let (id, raw) = row?;
                let images = raw.as_deref().and_then(decode_images).unwrap_or_default();
                if !images.is_empty() {
                    out.insert(id, images);
                }
            }
            Ok(out)
        })
    }

    /// 追加一轮助手回答：**消息与「顶起会话时间」在同一个事务里**。
    ///
    /// 拆成两条独立语句会有中间失败窗口，留下「消息写进去了、会话时间没更新」
    /// 的不一致状态 —— 表现为新回答不能在侧栏冒到顶部，而且不会报任何错。
    #[allow(clippy::too_many_arguments)]
    pub fn append_assistant_turn(
        &self,
        conversation_id: &str,
        content: &str,
        reasoning: Option<&str>,
        provider_id: &str,
        model_key: &str,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        status: MessageStatus,
        error: Option<&AppError>,
    ) -> AppResult<String> {
        let id = new_id("msg_");
        let now = now_ms();
        // 失败详情序列化成 JSON 存一列；读的时候再解析回 `AppError`。
        let error_json = error
            .map(|e| serde_json::to_string(e).unwrap_or_default())
            .or_else(|| (status == MessageStatus::Error).then(|| "{}".to_string()));
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO messages
                   (id, conversation_id, role, content, reasoning, provider_id,
                    model_key, input_tokens, output_tokens, created_at, status, error_json)
                 VALUES (?1, ?2, 'assistant', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    id,
                    conversation_id,
                    content,
                    reasoning,
                    provider_id,
                    model_key,
                    input_tokens,
                    output_tokens,
                    now,
                    status.as_str(),
                    error_json,
                ],
            )?;
            tx.execute(
                "UPDATE conversations
                    SET updated_at = ?2, provider_id = ?3, model_key = ?4
                  WHERE id = ?1",
                rusqlite::params![conversation_id, now, provider_id, model_key],
            )?;
            tx.commit()?;
            Ok(id)
        })
    }

    /// 改一条消息的正文。
    ///
    /// 顺带**作废该会话的滚动摘要**（同一个事务里）：摘要是对旧正文的压缩，
    /// 用户一改，摘要里描述的内容就可能已经不存在了。留着它会让模型看到
    /// 「摘要说用户叫小明」而实际那条消息已经改成别的。作废之后下一轮会重新压。
    pub fn update_message_content(&self, id: &str, content: &str) -> AppResult<()> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE messages SET content = ?2 WHERE id = ?1",
                rusqlite::params![id, content],
            )?;
            tx.execute(
                "UPDATE conversations
                    SET summary = '', summary_message_count = 0, summary_covered_message_id = NULL
                  WHERE id = (SELECT conversation_id FROM messages WHERE id = ?1)",
                [id],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    /// 删一条消息，并作废该会话的滚动摘要。
    ///
    /// 为什么删消息也要作废摘要：被删的可能正是摘要描述过的那段历史。
    /// （切上下文本身已经改成按 id 定位、找不到就放弃摘要，不会再丢用户的提问；
    /// 但摘要内容对不上仍然是错的，索性让它重建。）
    pub fn delete_message(&self, id: &str) -> AppResult<()> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE conversations
                    SET summary = '', summary_message_count = 0, summary_covered_message_id = NULL
                  WHERE id = (SELECT conversation_id FROM messages WHERE id = ?1)",
                [id],
            )?;
            tx.execute("DELETE FROM messages WHERE id = ?1", [id])?;
            tx.commit()?;
            Ok(())
        })
    }

    /// 取某条消息之前（不含）的历史，用于「编辑后重跑」。
    pub fn messages_before(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> AppResult<Vec<StoredMessage>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, conversation_id, role, content, reasoning, provider_id,
                        model_key, input_tokens, output_tokens, created_at, status, error_json,
                        images_json
                   FROM messages
                  WHERE conversation_id = ?1
                    AND rowid < (SELECT rowid FROM messages WHERE id = ?2)
                  ORDER BY created_at ASC, rowid ASC",
            )?;
            let rows =
                stmt.query_map(rusqlite::params![conversation_id, message_id], map_message)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// 每月 token 用量（给设置里的迷你统计用）。
    pub fn token_totals(&self) -> AppResult<(i64, i64)> {
        self.with_conn(|conn| {
            let row = conn.query_row(
                "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0)
                   FROM messages",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )?;
            Ok(row)
        })
    }

    // ---------------------------------------------------------------- 搜索

    /// 跨会话正文搜索。
    ///
    /// trigram 分词器要求关键词至少 3 个字符才有索引可用，更短的词退化成
    /// `LIKE`（会全表扫，但短词搜索本来就少、结果集也小）。
    pub fn search_messages(&self, query: &str, limit: i64) -> AppResult<Vec<MessageHit>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }

        // 用双引号包起来做「短语/子串」匹配，同时把查询里的双引号转义掉，
        // 否则用户输入 `"` 会让 MATCH 语法报错。
        let escaped = q.replace('"', "\"\"");
        let match_expr = format!("\"{escaped}\"");

        self.with_conn(|conn| {
            let mut out = Vec::new();

            if q.chars().count() >= 3 {
                let mut stmt = conn.prepare(
                    "SELECT m.id, m.conversation_id, c.title, m.role, m.created_at,
                            snippet(messages_fts, 0, '[', ']', '…', 12)
                       FROM messages_fts
                       JOIN messages m      ON m.rowid = messages_fts.rowid
                       JOIN conversations c ON c.id = m.conversation_id
                      WHERE messages_fts MATCH ?1
                      ORDER BY bm25(messages_fts)
                      LIMIT ?2",
                )?;
                let rows = stmt.query_map(rusqlite::params![match_expr, limit], |row| {
                    Ok(MessageHit {
                        message_id: row.get(0)?,
                        conversation_id: row.get(1)?,
                        conversation_title: row.get(2)?,
                        role: row.get(3)?,
                        created_at: row.get(4)?,
                        snippet: row.get(5)?,
                    })
                })?;
                for row in rows {
                    out.push(row?);
                }
                if !out.is_empty() {
                    return Ok(out);
                }
            }

            // 回退路径：短词，或 trigram 没命中（例如含 emoji 的查询）。
            //
            // 转义顺序很关键：**必须先转义反斜杠本身**。否则用户搜 `a\` 时
            // 拼出来的模式是 `%a\%`，末尾那个 `\%` 会被 ESCAPE 解释成
            // 「字面量百分号」，于是搜的是「a 后面跟一个 %」而不是「a 后面跟
            // 一个反斜杠」—— 结果是静默地返回错误集合。
            let like = format!("%{}%", escape_like(q));
            let mut stmt = conn.prepare(
                "SELECT m.id, m.conversation_id, c.title, m.role, m.created_at,
                        substr(m.content, 1, 120)
                   FROM messages m
                   JOIN conversations c ON c.id = m.conversation_id
                  WHERE m.content LIKE ?1 ESCAPE '\\'
                  ORDER BY m.created_at DESC
                  LIMIT ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![like, limit], |row| {
                Ok(MessageHit {
                    message_id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    conversation_title: row.get(2)?,
                    role: row.get(3)?,
                    created_at: row.get(4)?,
                    snippet: row.get(5)?,
                })
            })?;
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }
}

/// 把用户输入转义成「字面量 LIKE 模式」的一部分。
///
/// `LIKE ... ESCAPE '\'` 的语义是：反斜杠把下一个字符变成字面量。
/// 所以反斜杠自己必须先被转义成 `\\`，否则后面两个替换产生的反斜杠会被
/// 二次解释。顺序错了会静默返回错误的结果集（不报错，最难查）。
fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '%' => out.push_str("\\%"),
            '_' => out.push_str("\\_"),
            other => out.push(other),
        }
    }
    out
}

fn map_conversation(row: &rusqlite::Row<'_>) -> rusqlite::Result<Conversation> {
    Ok(Conversation {
        id: row.get(0)?,
        title: row.get(1)?,
        provider_id: row.get(2)?,
        model_key: row.get(3)?,
        system_prompt: row.get(4)?,
        pinned: row.get::<_, i64>(5)? != 0,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        message_count: row.get(8)?,
        summary: row.get(9)?,
        summary_message_count: row.get(10)?,
        summary_covered_message_id: row.get(11)?,
        role_id: row.get(12)?,
    })
}

fn map_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMessage> {
    let error_json: Option<String> = row.get(11)?;
    Ok(StoredMessage {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        reasoning: row.get(4)?,
        provider_id: row.get(5)?,
        model_key: row.get(6)?,
        input_tokens: row.get(7)?,
        output_tokens: row.get(8)?,
        created_at: row.get(9)?,
        status: MessageStatus::from_db(&row.get::<_, String>(10)?),
        error: error_json.and_then(|s| serde_json::from_str(&s).ok()),
        images: row
            .get::<_, Option<String>>(12)?
            .as_deref()
            .and_then(decode_images)
            .unwrap_or_default(),
    })
}

/// 把图片列表序列化进 `images_json`；空列表存 `NULL`（而不是 `"[]"`），
/// 这样 `WHERE images_json IS NOT NULL` 就能直接筛出「有图的消息」。
fn encode_images(images: &[String]) -> AppResult<Option<String>> {
    if images.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(images)?))
}

/// 解析 `images_json`。内容坏掉时返回 `None`（当成没有图片），
/// 不让一条脏数据把整个会话读不出来。
fn decode_images(raw: &str) -> Option<Vec<String>> {
    serde_json::from_str::<Vec<String>>(raw).ok()
}

/// 逐条执行未应用的迁移，每一步都在自己的事务里。
fn migrate(conn: &Connection) -> AppResult<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let target = target_version();

    if current > target {
        // 用旧版本程序打开新版本的库：继续跑只会写坏数据，直接拒绝。
        return Err(AppError::db(format!(
            "数据库版本（v{current}）高于当前程序支持的版本（v{target}），请升级应用后再打开。"
        )));
    }

    for (index, step) in MIGRATIONS.iter().enumerate() {
        let version = index as i64 + 1;
        if version <= current {
            continue;
        }
        let tx = conn.unchecked_transaction()?;
        match step {
            Step::Sql(sql) => tx.execute_batch(sql)?,
            Step::AddColumn {
                table,
                column,
                decl,
            } => {
                if !column_exists(&tx, table, column)? {
                    // 表名/列名/类型都来自编译期常量，不存在注入面。
                    tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
                }
            }
        }
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> AppResult<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (Database, PathBuf) {
        let dir = std::env::temp_dir().join(format!("floatai-db-{}", new_id("")));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chat.db");
        (Database::open(&path).unwrap(), dir)
    }

    #[test]
    fn a_conversation_remembers_its_role_and_can_clear_it() {
        let (db, dir) = temp_db();
        let conv = db
            .create_conversation("deepseek", "deepseek-chat", None)
            .unwrap();
        // 新会话默认没有人设（回落到全局系统提示词）
        assert_eq!(conv.role_id, None);

        db.set_conversation_role(&conv.id, Some("builtin-coder"))
            .unwrap();
        assert_eq!(
            db.get_conversation(&conv.id)
                .unwrap()
                .unwrap()
                .role_id
                .as_deref(),
            Some("builtin-coder")
        );
        // 列表里也必须带上：切走再切回来时界面要能显示「当前是哪个角色」，
        // 只在 get_conversation 里有的话，列表驱动的界面会显示成「无角色」。
        assert_eq!(
            db.list_conversations(10).unwrap()[0].role_id.as_deref(),
            Some("builtin-coder")
        );

        db.set_conversation_role(&conv.id, None).unwrap();
        assert_eq!(
            db.get_conversation(&conv.id).unwrap().unwrap().role_id,
            None
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn switching_role_does_not_bump_the_conversation_to_the_top() {
        // 换个角色不是「有新内容」。顶了 updated_at 的话，一个几个月前的旧会话
        // 会因为「改了个人设」跳到列表最前面 —— 用户会以为里面来了新消息。
        let (db, dir) = temp_db();
        let conv = db.create_conversation("p", "m", None).unwrap();
        let before = db.get_conversation(&conv.id).unwrap().unwrap().updated_at;
        std::thread::sleep(std::time::Duration::from_millis(5));
        db.set_conversation_role(&conv.id, Some("builtin-writer"))
            .unwrap();
        let after = db.get_conversation(&conv.id).unwrap().unwrap().updated_at;
        assert_eq!(before, after, "换人设不该改 updated_at");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_backup_round_trips_through_export_and_import() {
        let (db, dir) = temp_db();
        let conv = db
            .create_conversation("deepseek", "deepseek-chat", Some("备份测试"))
            .unwrap();
        db.set_conversation_role(&conv.id, Some("builtin-coder"))
            .unwrap();
        db.insert_message(&conv.id, "user", "问题", &[], None, None, None, None, None)
            .unwrap();
        db.insert_message(
            &conv.id,
            "assistant",
            "回答",
            &[],
            Some("思路"),
            None,
            None,
            Some(3),
            Some(4),
        )
        .unwrap();

        let backup = db.export_backup().unwrap();
        assert_eq!(backup.conversations.len(), 1);
        assert_eq!(backup.conversations[0].messages.len(), 2);
        assert_eq!(
            backup.conversations[0].role_id.as_deref(),
            Some("builtin-coder")
        );
        assert_eq!(backup.app, crate::storage::backup::BACKUP_APP);

        // ① 同一个库再导入一次：全部按 id 跳过，绝不产生重复
        let again = db.import_backup(&backup).unwrap();
        assert_eq!(again.imported, 0);
        assert_eq!(again.skipped, 1);
        assert_eq!(db.list_messages(&conv.id).unwrap().len(), 2, "不能导重");

        // ② 导进一个空库（换机器 / 重装后的真实路径）：完整还原
        let (fresh, dir2) = temp_db();
        let report = fresh.import_backup(&backup).unwrap();
        assert_eq!(report.imported, 1);
        assert_eq!(report.messages, 2);

        let restored = fresh.get_conversation(&conv.id).unwrap().unwrap();
        assert_eq!(restored.title, "备份测试");
        assert_eq!(restored.role_id.as_deref(), Some("builtin-coder"));

        let messages = fresh.list_messages(&conv.id).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content, "回答");
        assert_eq!(messages[1].reasoning.as_deref(), Some("思路"));
        assert_eq!(messages[1].output_tokens, Some(4));

        // ③ 全文索引要跟着恢复的数据一起工作（触发器必须对导入生效，
        //    否则「恢复了但搜不到」—— 那比没恢复更让人困惑）
        assert_eq!(
            fresh.search_messages("回答", 10).unwrap().len(),
            1,
            "导入的消息要能被搜到"
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&dir2).ok();
    }

    #[test]
    fn usage_aggregation_counts_only_assistant_messages_that_reported_tokens() {
        let (db, dir) = temp_db();
        let now = now_ms();
        let conv = db
            .create_conversation("deepseek", "deepseek-chat", None)
            .unwrap();

        // 用户消息：没有 token —— 混进用量会造出「模型名为空」的行，
        // 然后被费用估算列成「认不出来的模型」，纯噪音。
        db.insert_message(&conv.id, "user", "问", &[], None, None, None, None, None)
            .unwrap();
        // 助手消息，报过 usage：要算
        db.insert_message(
            &conv.id,
            "assistant",
            "答",
            &[],
            None,
            Some("deepseek"),
            Some("deepseek-chat"),
            Some(1000),
            Some(500),
        )
        .unwrap();
        // 助手消息但完全没有 usage（被取消 / 失败）：不算钱
        db.insert_message(
            &conv.id,
            "assistant",
            "（被取消）",
            &[],
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let rows = db.usage_by_day_model(now - 86_400_000).unwrap();
        assert_eq!(rows.len(), 1, "只该有一行（那天 + 那个模型）：{rows:?}");
        assert_eq!(rows[0].input_tokens, 1000);
        assert_eq!(rows[0].output_tokens, 500);
        assert_eq!(rows[0].messages, 1, "只数真的报了 usage 的那一条");
        assert_eq!(rows[0].model_key, "deepseek-chat");
        // 日期要按本地时间分好组，不能是空串
        assert_eq!(
            rows[0].day.len(),
            10,
            "期望 YYYY-MM-DD，实际 {}",
            rows[0].day
        );

        // 时间窗口之外不参与统计。
        //
        // 用「一分钟后」而不是「一毫秒后」：消息的 `created_at` 是插入那一刻的
        // 时间戳，几次插入本身就要花几毫秒 —— 取 `now + 1` 会把刚插进去的消息
        // 也算进窗口里，测试自己把自己坑了（第一次就是这么挂的）。
        assert!(
            db.usage_by_day_model(now + 60_000).unwrap().is_empty(),
            "since 在未来就不该有任何行"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn usage_aggregation_separates_models_and_sums_the_same_day() {
        // 同一天的两个模型必须是两行（价格不同，不能先合并再计价）。
        let (db, dir) = temp_db();
        let now = now_ms();
        let conv = db.create_conversation("p", "m", None).unwrap();
        for (model, input) in [("deepseek-chat", 1000_i64), ("gpt-4o", 2000)] {
            db.insert_message(
                &conv.id,
                "assistant",
                "答",
                &[],
                None,
                Some("p"),
                Some(model),
                Some(input),
                Some(100),
            )
            .unwrap();
        }
        let rows = db.usage_by_day_model(now - 86_400_000).unwrap();
        assert_eq!(rows.len(), 2, "不同模型要分开：{rows:?}");
        let chat = rows
            .iter()
            .find(|r| r.model_key == "deepseek-chat")
            .unwrap();
        let gpt = rows.iter().find(|r| r.model_key == "gpt-4o").unwrap();
        assert_eq!(chat.input_tokens, 1000);
        assert_eq!(gpt.input_tokens, 2000);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn importing_a_conversation_without_a_title_gets_a_default_one() {
        // 手改过的备份文件可能没有标题；空标题会让会话在列表里变成一个
        // 看不见名字的条目。
        let (db, dir) = temp_db();
        let mut backup = db.export_backup().unwrap();
        backup.conversations.push(BackupConversation {
            id: "conv_imported".to_string(),
            title: "   ".to_string(),
            provider_id: "p".to_string(),
            model_key: "m".to_string(),
            role_id: None,
            pinned: false,
            created_at: 1,
            updated_at: 2,
            summary: String::new(),
            messages: vec![],
        });
        db.import_backup(&backup).unwrap();
        assert_eq!(
            db.get_conversation("conv_imported").unwrap().unwrap().title,
            DEFAULT_CONVERSATION_TITLE
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn kb_search_finds_the_right_document_in_chinese_and_english() {
        let (db, dir) = temp_db();
        db.kb_add_document(
            "Rust 所有权",
            "rust.md",
            "Rust 的所有权系统在编译期检查内存安全，借用检查器会拒绝悬垂引用。",
        )
        .unwrap();
        db.kb_add_document(
            "浮语使用说明",
            "manual.md",
            "浮语支持全局热键 Alt+Space 唤出悬浮窗，也可以用划词助手 Alt+Q 提问。",
        )
        .unwrap();
        db.kb_add_document("菜谱", "cook.md", "番茄炒蛋：先打蛋，再下锅翻炒。")
            .unwrap();

        // 中文检索（走单字 + 双字索引）
        let hits = db.kb_search("借用检查器", 5).unwrap();
        assert!(!hits.is_empty(), "中文检索不该空手而归");
        assert_eq!(hits[0].title, "Rust 所有权");
        assert_eq!(hits[0].ordinal, 1, "只有一片，应当是第 1 片");

        // 英文 / 符号混排
        let hits = db.kb_search("Alt+Space", 5).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].title, "浮语使用说明");

        // 单字检索（只入 bigram 的实现在这里会失败）
        let hits = db.kb_search("蛋", 5).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].title, "菜谱");

        // 不相关的查询不该硬凑结果（这几个字都不在任何文档里）
        assert!(db.kb_search("量子隧穿", 5).unwrap().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn kb_search_prefers_the_more_specific_document() {
        let (db, dir) = temp_db();
        // 「知识库」在两篇里都出现，但第二篇更集中（出现次数多）
        db.kb_add_document(
            "泛泛而谈",
            "a.md",
            "这是一个知识库。它还提到别的很多东西，比如天气、交通、股票。",
        )
        .unwrap();
        db.kb_add_document(
            "知识库专论",
            "b.md",
            "知识库。知识库的检索。知识库的排序。知识库的切块。",
        )
        .unwrap();

        let hits = db.kb_search("知识库", 5).unwrap();
        assert!(hits.len() >= 2);
        assert_eq!(hits[0].title, "知识库专论", "命中最集中的应当排第一");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn kb_delete_removes_postings_and_recomputes_document_frequency() {
        let (db, dir) = temp_db();
        let keep = db
            .kb_add_document("保留", "keep.md", "浮语支持插件。")
            .unwrap();
        let drop = db
            .kb_add_document("删除", "drop.md", "浮语支持插件。独一无二的词：鲸落。")
            .unwrap();

        let before = db.kb_stats().unwrap();
        assert_eq!(before.documents, 2);

        db.kb_delete_document(&drop.id).unwrap();

        // ① 被删文档独有的词不该还能搜到
        assert!(
            db.kb_search("鲸落", 5).unwrap().is_empty(),
            "被删文档的词还在倒排表里"
        );

        // ② 两篇都有的词仍然命中保留的那篇
        let hits = db.kb_search("浮语 插件", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].document_id, keep.id);

        // ③ 统计与倒排表都跟着缩小（df 重算过，不该留下孤儿词）
        let after = db.kb_stats().unwrap();
        assert_eq!(after.documents, 1);
        assert!(after.chunks < before.chunks);
        assert!(
            after.terms < before.terms,
            "只存在于被删文档里的词应当被清掉"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn kb_add_rejects_empty_documents_with_a_clear_message() {
        let (db, dir) = temp_db();
        let blank = db.kb_add_document("标题", "x.md", "   \n\n  ").unwrap_err();
        assert!(blank.message.contains("内容为空"), "{}", blank.message);
        let no_title = db.kb_add_document("  ", "x.md", "有内容").unwrap_err();
        assert!(no_title.message.contains("标题"), "{}", no_title.message);
        // 拒收之后库里不该留下半份数据
        assert_eq!(db.kb_stats().unwrap().documents, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn kb_stats_and_document_list_reflect_what_was_added() {
        let (db, dir) = temp_db();
        assert_eq!(db.kb_document_count().unwrap(), 0);

        // 一段足够长、会被切成多片的文字
        let long = "浮语的知识库测试。".repeat(200);
        let doc = db.kb_add_document("长文", "long.md", &long).unwrap();
        assert!(doc.chunk_count > 1, "这么长的文字应当被切成多片");
        assert_eq!(doc.chars, long.chars().count() as i64);

        assert_eq!(db.kb_document_count().unwrap(), 1);
        let list = db.kb_list_documents().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "长文");

        let stats = db.kb_stats().unwrap();
        assert_eq!(stats.documents, 1);
        assert_eq!(stats.chunks, doc.chunk_count);
        assert!(stats.terms > 0);
        assert_eq!(stats.chars, doc.chars);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn kb_search_on_an_empty_library_is_empty_not_an_error() {
        let (db, dir) = temp_db();
        assert!(db.kb_search("随便什么", 5).unwrap().is_empty());
        // 空查询也不该报错
        assert!(db.kb_search("   ", 5).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_existing_database_upgrades_in_place_without_losing_data() {
        // 从「已经装过旧版本、库里有数据」的状态升级 —— 这才是真实用户路径。
        // 上面的 `migrations_reach_the_target_version` 是从空库一路建起来，
        // 覆盖不到「表已存在、只补新列」这一步。
        let (db, dir) = temp_db();

        let conv = db
            .create_conversation("deepseek", "deepseek-chat", None)
            .unwrap();
        let first = db
            .insert_message(
                &conv.id,
                "user",
                "升级前的旧消息",
                &[],
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        db.insert_message(
            &conv.id,
            "assistant",
            "升级前的旧回答",
            &[],
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        db.set_conversation_summary(&conv.id, "升级前的旧摘要", 2, Some(&first))
            .unwrap();
        drop(db);

        // 把库退回「上一个版本」的样子：删掉 v12 才有的列、把版本号改回 11。
        {
            let conn = rusqlite::Connection::open(dir.join("chat.db")).unwrap();
            conn.execute_batch(
                "ALTER TABLE conversations DROP COLUMN summary_covered_message_id;
                 PRAGMA user_version = 11;",
            )
            .unwrap();
        }

        // 重新打开 → 迁移应当把列补回来。
        let upgraded = Database::open(&dir.join("chat.db")).unwrap();
        let version: i64 = upgraded
            .with_conn(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(version, target_version(), "版本号要跟上");

        // 旧数据一条都不能丢。
        let loaded = upgraded.get_conversation(&conv.id).unwrap().unwrap();
        assert_eq!(loaded.title, conv.title);
        assert_eq!(loaded.summary, "升级前的旧摘要");
        assert_eq!(loaded.summary_message_count, 2);
        // 新列存在但为空 —— 旧库里本来就没有这个信息，`fit_context_with_summary`
        // 会因为定位不到边界而放弃使用这份旧摘要（安全的一侧）。
        assert_eq!(loaded.summary_covered_message_id, None);
        assert_eq!(upgraded.list_messages(&conv.id).unwrap().len(), 2);

        // 补写的摘要要能正常带上边界 id。
        upgraded
            .set_conversation_summary(&conv.id, "升级后的新摘要", 2, Some(&first))
            .unwrap();
        assert_eq!(
            upgraded
                .get_conversation(&conv.id)
                .unwrap()
                .unwrap()
                .summary_covered_message_id
                .as_deref(),
            Some(first.as_str())
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migrations_reach_the_target_version() {
        let (db, dir) = temp_db();
        let version: i64 = db
            .with_conn(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(version, target_version());

        // 再开一次：迁移必须幂等，不能报 duplicate column。
        drop(db);
        let again = Database::open(&dir.join("chat.db")).unwrap();
        let version2: i64 = again
            .with_conn(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(version2, target_version());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rolling_summary_round_trips_and_starts_empty() {
        let (db, dir) = temp_db();
        let conv = db
            .create_conversation("deepseek", "deepseek-chat", None)
            .unwrap();

        // 新会话没有摘要。
        let fresh = db.get_conversation(&conv.id).unwrap().unwrap();
        assert_eq!(fresh.summary, "");
        assert_eq!(fresh.summary_message_count, 0);
        assert_eq!(fresh.summary_covered_message_id, None);
        assert_eq!(conv.summary, "");

        db.set_conversation_summary(&conv.id, "用户在做读书笔记。", 30, Some("msg_30"))
            .unwrap();

        // 读回来（两条不同的查询路径都要带上摘要）。
        let loaded = db.get_conversation(&conv.id).unwrap().unwrap();
        assert_eq!(loaded.summary, "用户在做读书笔记。");
        assert_eq!(loaded.summary_message_count, 30);
        assert_eq!(
            loaded.summary_covered_message_id.as_deref(),
            Some("msg_30"),
            "边界 id 必须一起存下来 —— 下一轮切上下文靠它定位"
        );

        let listed = db.list_conversations(50).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].summary, "用户在做读书笔记。");
        assert_eq!(listed[0].summary_message_count, 30);
        assert_eq!(
            listed[0].summary_covered_message_id.as_deref(),
            Some("msg_30")
        );

        // 增量更新：覆盖条数往前走，正文与边界都被替换。
        db.set_conversation_summary(
            &conv.id,
            "用户在做读书笔记，已读到第三章。",
            42,
            Some("msg_42"),
        )
        .unwrap();
        let again = db.get_conversation(&conv.id).unwrap().unwrap();
        assert_eq!(again.summary, "用户在做读书笔记，已读到第三章。");
        assert_eq!(again.summary_message_count, 42);
        assert_eq!(again.summary_covered_message_id.as_deref(), Some("msg_42"));

        // 边界可以清空（例如摘要被作废）。
        db.set_conversation_summary(&conv.id, "", 0, None).unwrap();
        let cleared = db.get_conversation(&conv.id).unwrap().unwrap();
        assert_eq!(cleared.summary, "");
        assert_eq!(cleared.summary_covered_message_id, None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn images_round_trip_and_batch_lookup() {
        let (db, dir) = temp_db();
        let conv = db.create_conversation("p", "m", None).unwrap();

        let images = vec![
            "data:image/png;base64,AAAA".to_string(),
            "data:image/jpeg;base64,BBBB".to_string(),
        ];
        let with_image = db
            .insert_message(
                &conv.id,
                "user",
                "看这张图",
                &images,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let without = db
            .insert_message(
                &conv.id,
                "user",
                "没有图",
                &[],
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();

        // 列表读回来时图片还在，且顺序不变。
        let messages = db.list_messages(&conv.id).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].images, images);
        assert_eq!(messages[0].content, "看这张图");
        assert!(messages[1].images.is_empty());

        // 批量查：只返回有图的那条（无图的存的是 NULL）。
        let found = db
            .images_for_ids(&[
                with_image.clone(),
                without.clone(),
                "msg_不存在".to_string(),
            ])
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found.get(&with_image).unwrap(), &images);

        // 空入参不该去查库。
        assert!(db.images_for_ids(&[]).unwrap().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_corrupt_images_column_degrades_to_no_images() {
        let (db, dir) = temp_db();
        let conv = db.create_conversation("p", "m", None).unwrap();
        let id = db
            .insert_message(
                &conv.id,
                "user",
                "有图",
                &["data:image/png;base64,AA".to_string()],
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();

        // 手动写坏这一列：一条脏数据不该让整个会话读不出来。
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE messages SET images_json = 'not json' WHERE id = ?1",
                [&id],
            )?;
            Ok(())
        })
        .unwrap();

        let messages = db.list_messages(&conv.id).unwrap();
        assert_eq!(messages.len(), 1, "消息本身仍然要能读出来");
        assert_eq!(messages[0].content, "有图");
        assert!(messages[0].images.is_empty(), "坏数据要退化成「没有图片」");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn editing_or_deleting_a_message_invalidates_the_summary() {
        let (db, dir) = temp_db();
        let conv = db.create_conversation("p", "m", None).unwrap();
        let first = db
            .insert_message(
                &conv.id,
                "user",
                "第一问",
                &[],
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        db.insert_message(
            &conv.id,
            "assistant",
            "第一答",
            &[],
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        db.set_conversation_summary(&conv.id, "用户问了第一问。", 2, Some(&first))
            .unwrap();
        assert_eq!(
            db.get_conversation(&conv.id).unwrap().unwrap().summary,
            "用户问了第一问。"
        );

        // 改一条消息 → 摘要必须作废：它描述的是**旧正文**，
        // 留着会让模型以为用户还说过那句话。
        db.update_message_content(&first, "完全换了一句话").unwrap();
        let after_edit = db.get_conversation(&conv.id).unwrap().unwrap();
        assert_eq!(after_edit.summary, "", "改消息后摘要要作废");
        assert_eq!(after_edit.summary_message_count, 0);
        assert_eq!(after_edit.summary_covered_message_id, None);

        // 重建摘要后删一条消息 → 同样作废。
        db.set_conversation_summary(&conv.id, "又一份摘要", 2, Some(&first))
            .unwrap();
        db.delete_message(&first).unwrap();
        let after_delete = db.get_conversation(&conv.id).unwrap().unwrap();
        assert_eq!(after_delete.summary, "", "删消息后摘要也要作废");
        assert_eq!(after_delete.summary_covered_message_id, None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn images_are_only_loaded_for_the_newest_messages() {
        let (db, dir) = temp_db();
        let conv = db.create_conversation("p", "m", None).unwrap();

        let total = IMAGE_HISTORY_LIMIT as usize + 3;
        let mut ids = Vec::new();
        for index in 0..total {
            let id = db
                .insert_message(
                    &conv.id,
                    "user",
                    &format!("第 {index} 条"),
                    &[format!("data:image/png;base64,IMG{index}")],
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
            ids.push(id);
        }

        let messages = db.list_messages(&conv.id).unwrap();
        assert_eq!(messages.len(), total, "正文一条都不能少");

        // 最早的三条不再带图 —— 这是**界面**的取舍：避免一次加载几百 MB。
        for (index, message) in messages.iter().take(3).enumerate() {
            assert!(message.images.is_empty(), "第 {index} 条的图片不该被加载");
            assert_eq!(message.content, format!("第 {index} 条"), "正文必须还在");
        }
        assert_eq!(messages.last().unwrap().images.len(), 1);

        // 但按 id 取图**不受**这个限制：模型发请求走的是这条路，
        // 所以「老图片在界面上看不到」不等于「模型看不到」。
        let all = db.images_for_ids(&ids).unwrap();
        assert_eq!(all.len(), total, "按 id 取图必须能拿到全部");
        assert!(all.contains_key(&ids[0]), "包含最早的那条");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn conversation_crud_and_cascade() {
        let (db, dir) = temp_db();
        let conv = db
            .create_conversation("deepseek", "deepseek-chat", None)
            .unwrap();
        db.insert_message(&conv.id, "user", "你好", &[], None, None, None, None, None)
            .unwrap();
        db.insert_message(
            &conv.id,
            "assistant",
            "在的",
            &[],
            None,
            None,
            None,
            Some(3),
            Some(5),
        )
        .unwrap();

        let listed = db.list_conversations(50).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].message_count, 2);

        let messages = db.list_messages(&conv.id).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");

        assert_eq!(db.token_totals().unwrap(), (3, 5));

        db.delete_conversation(&conv.id).unwrap();
        assert!(db.list_conversations(50).unwrap().is_empty());
        assert!(db.list_messages(&conv.id).unwrap().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chinese_full_text_search_hits() {
        let (db, dir) = temp_db();
        let conv = db.create_conversation("p", "m", Some("测试")).unwrap();
        db.insert_message(
            &conv.id,
            "user",
            "帮我看看这个 Rust 的所有权问题",
            &[],
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        db.insert_message(
            &conv.id,
            "assistant",
            "所有权是编译期的概念",
            &[],
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let hits = db.search_messages("所有权", 10).unwrap();
        assert!(!hits.is_empty(), "trigram 应当能命中中文子串");
        assert!(hits[0].snippet.contains('['));

        // 两字词走 LIKE 回退。
        let short = db.search_messages("编译", 10).unwrap();
        assert!(!short.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deleting_a_conversation_clears_its_search_index() {
        let (db, dir) = temp_db();
        let conv = db.create_conversation("p", "m", None).unwrap();
        db.insert_message(
            &conv.id,
            "user",
            "独一无二的词组内容",
            &[],
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(!db.search_messages("独一无二", 10).unwrap().is_empty());

        db.delete_conversation(&conv.id).unwrap();
        assert!(
            db.search_messages("独一无二", 10).unwrap().is_empty(),
            "级联删除后 FTS 索引也必须同步清掉"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn escape_like_neutralises_wildcards() {
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        // 反斜杠必须**先**被转义，否则它会把自己后面那个字符吃掉。
        assert_eq!(escape_like("a\\"), "a\\\\");
        assert_eq!(escape_like("\\%"), "\\\\\\%");
        // 普通文字原样保留（含中文与 emoji）。
        assert_eq!(escape_like("你好🌍"), "你好🌍");
    }

    #[test]
    fn short_query_like_fallback_treats_wildcards_literally() {
        let (db, dir) = temp_db();
        let conv = db.create_conversation("p", "m", None).unwrap();
        let add = |text: &str| {
            db.insert_message(&conv.id, "user", text, &[], None, None, None, None, None)
                .unwrap();
        };
        add("完成度 99%");
        add("完成度 99X");
        add("路径 a\\b");

        // 两个字符 → 一定走 LIKE 回退分支。
        let percent = db.search_messages("9%", 10).unwrap();
        assert_eq!(
            percent.len(),
            1,
            "% 必须当字面量；当成通配符的话会把 99X 也搜出来"
        );
        assert!(percent[0].snippet.contains("99%"));

        // 查询以反斜杠结尾：转义漏了的话会变成「a 后面跟一个字面 %」，命中 0 条。
        let backslash = db.search_messages("a\\", 10).unwrap();
        assert_eq!(
            backslash.len(),
            1,
            "反斜杠必须被正确转义，实际命中 {} 条",
            backslash.len()
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_assistant_turn_writes_message_and_touches_conversation() {
        let (db, dir) = temp_db();
        let conv = db
            .create_conversation("deepseek", "deepseek-chat", None)
            .unwrap();
        // 让 now_ms() 确实前进，才能观察到 updated_at 的变化。
        std::thread::sleep(std::time::Duration::from_millis(5));

        db.append_assistant_turn(
            &conv.id,
            "这是回答",
            Some("这是思路"),
            "deepseek",
            "deepseek-reasoner",
            Some(11),
            Some(22),
            MessageStatus::Ok,
            None,
        )
        .unwrap();

        let messages = db.list_messages(&conv.id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "assistant");
        assert_eq!(messages[0].content, "这是回答");
        assert_eq!(messages[0].reasoning.as_deref(), Some("这是思路"));
        assert_eq!(messages[0].input_tokens, Some(11));
        assert_eq!(messages[0].output_tokens, Some(22));
        assert_eq!(messages[0].status, MessageStatus::Ok);

        let refreshed = db.get_conversation(&conv.id).unwrap().unwrap();
        assert!(
            refreshed.updated_at > conv.updated_at,
            "会话时间必须被顶起，否则新回答不会在侧栏冒到顶部"
        );
        assert_eq!(
            refreshed.model_key, "deepseek-reasoner",
            "要记住最后用的模型"
        );
        assert_eq!(refreshed.message_count, 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_errored_turn_round_trips_through_the_database() {
        let (db, dir) = temp_db();
        let conv = db.create_conversation("p", "m", None).unwrap();

        let err = AppError::new("rate_limited", "太频繁了").retryable(true);
        db.append_assistant_turn(
            &conv.id,
            "", // 空正文的失败轮也要落库 —— 它是「重试」的锚点
            None,
            "p",
            "m",
            None,
            None,
            MessageStatus::Error,
            Some(&err),
        )
        .unwrap();

        let messages = db.list_messages(&conv.id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].status, MessageStatus::Error);
        assert_eq!(messages[0].content, "");

        let back = messages[0].error.clone().expect("错误详情应当被还原");
        assert_eq!(back.code, "rate_limited");
        assert_eq!(back.message, "太频繁了");
        assert!(back.retryable);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_status_string_degrades_to_ok() {
        assert_eq!(MessageStatus::from_db("whatever"), MessageStatus::Ok);
        assert_eq!(
            MessageStatus::from_db("cancelled"),
            MessageStatus::Cancelled
        );
        assert_eq!(MessageStatus::from_db("error"), MessageStatus::Error);
    }

    #[test]
    fn messages_before_excludes_the_message_itself() {
        let (db, dir) = temp_db();
        let conv = db.create_conversation("p", "m", None).unwrap();
        let first = db
            .insert_message(
                &conv.id,
                "user",
                "第一问",
                &[],
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        db.insert_message(
            &conv.id,
            "assistant",
            "第一答",
            &[],
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let third = db
            .insert_message(
                &conv.id,
                "user",
                "第二问",
                &[],
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();

        let before = db.messages_before(&conv.id, &third).unwrap();
        assert_eq!(before.len(), 2, "只取该消息之前的历史");
        assert_eq!(before[0].id, first);

        let nothing = db.messages_before(&conv.id, &first).unwrap();
        assert!(nothing.is_empty(), "第一条消息之前没有历史");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_database_from_the_future_is_refused() {
        let dir = std::env::temp_dir().join(format!("floatai-db-future-{}", new_id("")));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chat.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", target_version() + 5)
                .unwrap();
        }
        let err = match Database::open(&path) {
            Ok(_) => panic!("更高的库版本必须被拒绝，而不是写坏数据"),
            Err(e) => e,
        };
        assert!(err.message.contains("高于"), "{}", err.message);

        std::fs::remove_dir_all(&dir).ok();
    }
}
