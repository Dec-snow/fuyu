// 清掉真机验证留下的测试数据。
//
// 为什么是**独立脚本**、而不是写在驱动的场景末尾：
// 场景中途失败时，末尾的清理根本不会执行 —— 这个项目里已经因此攒下过
// 3 个空会话、2 个带消息的测试会话、2 个知识库文档。清理必须
// **与场景成败无关**：跑测试前后各跑一次这个脚本就干净了。
//
// 判据（宁可少删，也不误删用户的东西）：
//   1. 标题命中测试标记（@tool / 端到端验证）的会话与知识库文档 —— 那是驱动发的固定文案；
//   2. **一条消息都没有**的空会话（用户在界面上点「+ 新对话」不会立刻建行，
//      所以空会话几乎一定来自测试中间态）。
// 有内容、标题又不匹配的会话一律不碰。
//
// 表名与列名用**运行时探测**，不写死：表结构改过一次（v14 加的知识库），
// 写死的话脚本会在某次迁移之后悄悄删错东西。
//
// 跑法：node tools/purge-test-data.mjs

import { DatabaseSync } from "node:sqlite";
import { homedir } from "node:os";
import { join } from "node:path";

const dbPath = join(homedir(), "AppData", "Roaming", "com.floatai.desktop", "floatai.db");
const db = new DatabaseSync(dbPath);

const TEST_TITLE = /@tool|端到端验证/;

/** 取出所有表名。 */
function tables() {
  return db
    .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
    .all()
    .map((row) => String(row.name));
}

/** 某张表有没有这一列。 */
function hasColumn(table, column) {
  try {
    return db
      .prepare(`PRAGMA table_info(${table})`)
      .all()
      .some((row) => String(row.name) === column);
  } catch {
    return false;
  }
}

let removedConversations = 0;
let removedEmpty = 0;
let removedDocs = 0;

// ---- 会话 ----
if (tables().includes("conversations")) {
  const rows = db
    .prepare(
      `SELECT c.id, c.title,
              (SELECT COUNT(*) FROM messages m WHERE m.conversation_id = c.id) AS n
       FROM conversations c`,
    )
    .all();
  for (const row of rows) {
    const isTestTitle = TEST_TITLE.test(String(row.title ?? ""));
    const isEmpty = Number(row.n) === 0;
    if (!isTestTitle && !isEmpty) continue;
    if (tables().includes("messages") && hasColumn("messages", "conversation_id")) {
      db.prepare("DELETE FROM messages WHERE conversation_id = ?").run(row.id);
    }
    db.prepare("DELETE FROM conversations WHERE id = ?").run(row.id);
    if (isTestTitle) removedConversations += 1;
    else removedEmpty += 1;
  }
}

// ---- 知识库文档（连同切片） ----
const docTable = tables().find((name) => name === "kb_documents" || name === "kb_docs");
if (docTable && hasColumn(docTable, "title")) {
  const docs = db
    .prepare(`SELECT id, title FROM ${docTable}`)
    .all()
    .filter((row) => TEST_TITLE.test(String(row.title ?? "")));
  for (const doc of docs) {
    for (const table of tables()) {
      if (hasColumn(table, "document_id")) {
        db.prepare(`DELETE FROM ${table} WHERE document_id = ?`).run(doc.id);
      }
    }
    db.prepare(`DELETE FROM ${docTable} WHERE id = ?`).run(doc.id);
    removedDocs += 1;
  }
}

const left = Number(db.prepare("SELECT COUNT(*) AS n FROM conversations").get().n);
const leftMessages = Number(db.prepare("SELECT COUNT(*) AS n FROM messages").get().n);
db.close();

console.log(
  `清掉测试会话 ${removedConversations} 个、空会话 ${removedEmpty} 个、知识库文档 ${removedDocs} 个；` +
    `剩下 会话 ${left}、消息 ${leftMessages}`,
);
