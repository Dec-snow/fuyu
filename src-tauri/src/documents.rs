//! Office 文档（docx / xlsx / pptx）的文字提取。
//!
//! 这三种格式本质都是 **ZIP + 一堆 XML**，所以不需要重量级依赖：`zip` 解包 +
//! `quick-xml` 流式扫 XML，只把文字捞出来。刻意**不**做版式还原（表格、样式、
//! 图片一律丢掉）—— 目标是「把内容喂给模型」，不是「渲染一份文档」。
//!
//! 三条安全约束（都是必需的，不是洁癖）：
//!
//! 1. **zip bomb 防护**：ZIP 能把几百 MB 膨胀成几十 GB。所以既限制条目数，
//!    也限制解压后总大小的**上限**，超了直接拒收。
//! 2. **文字长度上限**：提取出来的正文最终会进上下文预算，一个几 MB 的表格
//!    能把预算一次吃光。超了**截断并明确标注**，而不是悄悄丢。
//! 3. **按局部名匹配标签**：docx 的标准前缀是 `w:`，但生成器不一定守规矩。
//!    匹配 `local_name()`（前缀去掉后的名字）比硬编码前缀稳。
//!
//! ⚠️ 关于第 3 条的一个已知取舍：局部名 `t` 在 pptx 里是 `<a:t>`、在 docx 里是
//! `<w:t>`，两者都叫 `t`。我们一次只解析一种文件，所以不会串 —— 但如果将来
//! 要在同一个文件里同时认这两套，就必须把命名空间也带上。

use std::io::{Cursor, Read};

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::errors::{AppError, AppResult};

/// 允许的条目数上限（正常文档几十个，给足余量）。
pub const MAX_ENTRIES: usize = 2_000;
/// 解压后总大小上限（zip bomb 防护）。
pub const MAX_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;
/// 单个 XML 部件的大小上限。
pub const MAX_PART_BYTES: u64 = 32 * 1024 * 1024;
/// 提取出的正文字符数上限（与文本附件保持一致）。
pub const MAX_EXTRACTED_CHARS: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Docx,
    Xlsx,
    Pptx,
    /// PDF：不走 ZIP+XML 那条路（见 crate::pdf）。
    Pdf,
}

impl DocKind {
    /// 界面上显示的名字。
    pub fn label(self) -> &'static str {
        match self {
            DocKind::Docx => "Word 文档",
            DocKind::Xlsx => "Excel 表格",
            DocKind::Pptx => "PowerPoint 演示文稿",
            DocKind::Pdf => "PDF 文档",
        }
    }
}

/// 按扩展名判断是不是我们能解析的 Office 文档。
pub fn kind_of(file_name: &str) -> Option<DocKind> {
    let ext = file_name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())?;
    match ext.as_str() {
        "docx" => Some(DocKind::Docx),
        "xlsx" => Some(DocKind::Xlsx),
        "pptx" => Some(DocKind::Pptx),
        "pdf" => Some(DocKind::Pdf),
        _ => None,
    }
}

/// 提取正文。
pub fn extract(kind: DocKind, bytes: &[u8]) -> AppResult<String> {
    // PDF 不是 ZIP+XML，压根走不了下面那条路 —— 先分流。
    // （这也是为什么 `DocKind` 要把 Pdf 放进来而不是另开一个函数：
    //  调用方只该关心「这是个文档」，不该关心它内部是什么容器。）
    if kind == DocKind::Pdf {
        return crate::pdf::extract(bytes);
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| AppError::invalid(format!("不是有效的 Office 文档（打不开压缩包）：{e}")))?;

    check_size_limits(&mut archive)?;

    let text = match kind {
        DocKind::Docx => extract_docx(&mut archive)?,
        DocKind::Xlsx => extract_xlsx(&mut archive)?,
        DocKind::Pptx => extract_pptx(&mut archive)?,
        // 上面已经分流走了，这里只是让 match 穷尽
        DocKind::Pdf => unreachable!("PDF 在函数开头就分流了"),
    };

    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(AppError::invalid(format!(
            "这个{}里没有能提取的文字（可能是纯图片或空文档）",
            kind.label()
        )));
    }
    Ok(truncate_chars(text, MAX_EXTRACTED_CHARS))
}

/// 条目数与解压后大小的上限检查。
fn check_size_limits(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> AppResult<()> {
    if archive.len() > MAX_ENTRIES {
        return Err(AppError::invalid(format!(
            "压缩包里的条目太多（{} 个，上限 {MAX_ENTRIES}）",
            archive.len()
        )));
    }

    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| AppError::invalid(format!("读取压缩包条目失败：{e}")))?;
        total = total.saturating_add(entry.size());
        if total > MAX_UNCOMPRESSED_BYTES {
            return Err(AppError::invalid(format!(
                "解压后体积过大（超过 {} MB），已拒绝",
                MAX_UNCOMPRESSED_BYTES / 1_048_576
            )));
        }
    }
    Ok(())
}

/// 反转义一条**实体引用**（`Event::GeneralRef`）。
///
/// ⚠️ quick-xml 0.42 起把实体引用单独报成一个事件，不再混在 `Event::Text` 里。
/// 第一版只处理了 Text，后果是 openpyxl 写出来的表格里**整列文字凭空消失**
/// —— 因为它的内联字符串是 `&#21517;&#31216;` 这种数字引用，而数字列（纯 `<v>`）
/// 照常出现，看起来像「读到了但内容不对」。这类静默丢内容最难查，
/// 所以现在两种事件都收。
fn ref_text(raw: &quick_xml::events::BytesRef<'_>) -> String {
    match raw.resolve_char_ref() {
        // `&#21517;` / `&#x6B63;` 这类数字引用
        Ok(Some(ch)) => ch.to_string(),
        // 具名实体：XML 只预定义这五个
        Ok(None) => match raw.as_ref() {
            "amp" => "&".to_string(),
            "lt" => "<".to_string(),
            "gt" => ">".to_string(),
            "quot" => "\"".to_string(),
            "apos" => "'".to_string(),
            // 从网页/其它工具导出的文档里 `&nbsp;` 很常见：还原成空格，
            // 否则模型会读到一片 `&nbsp;`
            "nbsp" => " ".to_string(),
            // 其它未定义实体：**原样保留**而不是丢掉 —— 让模型看到「这里有点东西」，
            // 比凭空少一段强（也说明这份文档不是标准 XML）
            other => format!("&{other};"),
        },
        Err(_) => String::new(),
    }
}

/// 按名字读一个部件；不存在返回 `None`（不同生成器会省略不同的可选部件）。
fn read_part(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> AppResult<Option<String>> {
    let mut file = match archive.by_name(name) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(AppError::invalid(format!("读取 {name} 失败：{e}"))),
    };
    if file.size() > MAX_PART_BYTES {
        return Err(AppError::invalid(format!("{name} 太大，已拒绝")));
    }
    let mut raw = Vec::with_capacity(file.size().min(1_048_576) as usize);
    file.read_to_end(&mut raw)
        .map_err(|e| AppError::invalid(format!("解压 {name} 失败：{e}")))?;
    // Office 的 XML 一定是 UTF-8；真遇到别的编码也不该让整个导入失败。
    Ok(Some(String::from_utf8_lossy(&raw).into_owned()))
}

/// 收集 ZIP 里所有符合谓词的条目名（顺序稳定，便于 pptx 按页号排）。
fn part_names(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    predicate: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index_raw(i) else {
            continue;
        };
        // zip 9 的 `name()` 返回 Result（文件名可能不是合法 UTF-8）
        let Ok(raw) = entry.name() else {
            continue;
        };
        let name = raw.to_string();
        if predicate(&name) {
            names.push(name);
        }
    }
    names
}

/// 按字符数截断，并**明确标注**被截断了 —— 悄悄丢内容会让模型（和用户）
// 都以为看到的是全部。
fn truncate_chars(text: String, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text;
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}\n\n…（内容过长，已截断到 {limit} 字）")
}

// ---------------------------------------------------------------------------
// 通用：把 XML 里的文字捞出来
// ---------------------------------------------------------------------------

/// 一次遍历就能搞定的提取规则。
struct TextSpec<'a> {
    /// 这些局部名的元素结束时换行（段落 / 行）。
    newline_at: &'a [&'a str],
    /// 这些局部名里的文字要收集。
    text_in: &'a [&'a str],
    /// 这些局部名的空元素替换成什么（例如 `<w:br/>` → 换行）。
    empty_as: &'a [(&'a str, &'a str)],
}

/// 用 [`TextSpec`] 扫一遍 XML，返回纯文本。
fn walk_text(xml: &str, spec: &TextSpec<'_>) -> AppResult<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut out = String::new();
    let mut depth_text: usize = 0;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let local = local_name(e.name());
                if spec.text_in.contains(&local.as_str()) {
                    depth_text += 1;
                }
                if spec.newline_at.contains(&local.as_str()) {
                    // 段落开始时的换行由上一段的结束负责，这里不重复加
                }
            }
            Ok(Event::End(e)) => {
                let local = local_name(e.name());
                if spec.text_in.contains(&local.as_str()) {
                    depth_text = depth_text.saturating_sub(1);
                }
                if spec.newline_at.contains(&local.as_str()) && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            Ok(Event::Empty(e)) => {
                let local = local_name(e.name());
                if let Some((_, replacement)) = spec.empty_as.iter().find(|(n, _)| *n == local) {
                    out.push_str(replacement);
                }
            }
            Ok(Event::Text(t)) => {
                if depth_text > 0 {
                    // 实体引用已经由 reader 单独成事件，这里就是纯文本
                    out.push_str(t.as_ref());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if depth_text > 0 {
                    out.push_str(&ref_text(&r));
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => {
                return Err(AppError::invalid(format!(
                    "文档内容不是有效的 XML（第 {} 字节处）：{e}",
                    reader.buffer_position()
                )))
            }
        }
    }

    Ok(out)
}

/// `w:t` → `t`（去掉命名空间前缀）。
///
/// 直接复用 quick-xml 的 `QName::local_name()`，不自己切字符串。
fn local_name(name: quick_xml::name::QName<'_>) -> String {
    name.local_name().as_ref().to_string()
}

// ---------------------------------------------------------------------------
// docx
// ---------------------------------------------------------------------------

fn extract_docx(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> AppResult<String> {
    let xml = read_part(archive, "word/document.xml")?
        .ok_or_else(|| AppError::invalid("这个 docx 里找不到正文（word/document.xml）"))?;

    let spec = TextSpec {
        // 段落与表格行结束都换行；`w:p` 覆盖了绝大多数情况
        newline_at: &["p", "tr"],
        text_in: &["t"],
        empty_as: &[("br", "\n"), ("tab", "\t")],
    };
    walk_text(&xml, &spec)
}

// ---------------------------------------------------------------------------
// pptx
// ---------------------------------------------------------------------------

fn extract_pptx(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> AppResult<String> {
    let mut slides = part_names(archive, |name| {
        name.starts_with("ppt/slides/slide") && name.ends_with(".xml")
    });
    // slide2.xml 必须排在 slide10.xml 前面
    slides.sort_by_key(|name| slide_number(name));

    if slides.is_empty() {
        return Err(AppError::invalid("这个 pptx 里没有幻灯片"));
    }

    let spec = TextSpec {
        newline_at: &["p"],
        text_in: &["t"],
        empty_as: &[("br", "\n")],
    };

    let mut out = String::new();
    for (index, name) in slides.iter().enumerate() {
        if let Some(xml) = read_part(archive, name)? {
            let text = walk_text(&xml, &spec)?.trim().to_string();
            if text.is_empty() {
                continue;
            }
            // 标出页码：模型需要知道「哪句话在第几页」，否则一堆短句连在一起没法归位
            out.push_str(&format!("【幻灯片 {}】\n{text}\n\n", index + 1));
        }
    }
    Ok(out)
}

/// `ppt/slides/slide12.xml` → 12（取不到时给一个很大的数，排到最后）。
fn slide_number(name: &str) -> u32 {
    name.trim_start_matches("ppt/slides/slide")
        .trim_end_matches(".xml")
        .parse()
        .unwrap_or(u32::MAX)
}

// ---------------------------------------------------------------------------
// xlsx
// ---------------------------------------------------------------------------

/// 读 `xl/sharedStrings.xml`：单元格里的字符串大多存在这里，
/// 工作表里只留一个下标（`t="s"`）。这是 xlsx 最容易漏掉的一层间接。
fn shared_strings(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> AppResult<Vec<String>> {
    let Some(xml) = read_part(archive, "xl/sharedStrings.xml")? else {
        return Ok(Vec::new());
    };

    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(false);
    let mut items: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_si = false;
    let mut in_t = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let local = local_name(e.name());
                if local == "si" {
                    in_si = true;
                    current.clear();
                } else if local == "t" && in_si {
                    in_t = true;
                }
            }
            Ok(Event::End(e)) => {
                let local = local_name(e.name());
                if local == "t" {
                    in_t = false;
                } else if local == "si" {
                    items.push(std::mem::take(&mut current));
                    in_si = false;
                }
            }
            Ok(Event::Empty(_)) => {}
            Ok(Event::Text(t)) => {
                if in_t {
                    current.push_str(t.as_ref());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if in_t {
                    current.push_str(&ref_text(&r));
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => {
                return Err(AppError::invalid(format!(
                    "共享字符串表不是有效的 XML：{e}"
                )))
            }
        }
    }
    Ok(items)
}

/// 工作表名（`xl/workbook.xml` 里的 `<sheet name="...">`，按出现顺序）。
fn sheet_names(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> AppResult<Vec<String>> {
    let Some(xml) = read_part(archive, "xl/workbook.xml")? else {
        return Ok(Vec::new());
    };
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    let mut names = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if local_name(e.name()) == "sheet" {
                    for attr in e.attributes().flatten() {
                        if local_name(attr.key) == "name" {
                            // 0.42 的 `normalized_value` 需要显式给 XML 版本；
                            // OOXML 都是 XML 1.0，用默认值即可。
                            names.push(
                                attr.normalized_value(quick_xml::XmlVersion::default())
                                    .map(|v| v.into_owned())
                                    .unwrap_or_default(),
                            );
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break, // 表名读不出来不影响正文提取
        }
    }
    Ok(names)
}

fn extract_xlsx(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> AppResult<String> {
    let shared = shared_strings(archive)?;
    let names = sheet_names(archive)?;

    let mut sheets = part_names(archive, |name| {
        name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml")
    });
    sheets.sort_by_key(|name| sheet_number(name));

    if sheets.is_empty() {
        return Err(AppError::invalid("这个 xlsx 里没有工作表"));
    }

    let mut out = String::new();
    for (index, part) in sheets.iter().enumerate() {
        let Some(xml) = read_part(archive, part)? else {
            continue;
        };
        let title = names
            .get(index)
            .cloned()
            .unwrap_or_else(|| format!("工作表 {}", index + 1));
        let rows = read_sheet_rows(&xml, &shared);
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!("【{title}】\n"));
        for row in rows {
            out.push_str(&row);
            out.push('\n');
        }
        out.push('\n');
    }
    Ok(out)
}

fn sheet_number(name: &str) -> u32 {
    name.trim_start_matches("xl/worksheets/sheet")
        .trim_end_matches(".xml")
        .parse()
        .unwrap_or(u32::MAX)
}

/// 扫一个工作表，按行返回「用制表符分隔的单元格」。
fn read_sheet_rows(xml: &str, shared: &[String]) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut rows: Vec<String> = Vec::new();
    let mut cells: Vec<String> = Vec::new();
    let mut in_row = false;
    // 当前单元格：是不是共享字符串（t="s"）、值类型、以及收集到的文本
    let mut cell_is_shared = false;
    let mut cell_text = String::new();
    let mut in_value = false;
    let mut in_inline_text = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let local = local_name(e.name());
                match local.as_str() {
                    "row" => {
                        in_row = true;
                        cells.clear();
                    }
                    "c" => {
                        cell_is_shared = false;
                        cell_text.clear();
                        for attr in e.attributes().flatten() {
                            if local_name(attr.key) == "t" {
                                // quick-xml 0.42 起属性值是字符串（不再是字节）
                                cell_is_shared = attr.value.as_ref() == "s";
                            }
                        }
                    }
                    "v" => in_value = true,
                    "t" => in_inline_text = true,
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let local = local_name(e.name());
                match local.as_str() {
                    "v" => in_value = false,
                    "t" => in_inline_text = false,
                    "c" => {
                        // 共享字符串：`<v>` 里是下标
                        let value = if cell_is_shared {
                            cell_text
                                .trim()
                                .parse::<usize>()
                                .ok()
                                .and_then(|i| shared.get(i).cloned())
                                .unwrap_or_default()
                        } else {
                            cell_text.trim().to_string()
                        };
                        cells.push(value);
                    }
                    "row" if in_row => {
                        // 去掉行尾的空单元格（xlsx 里稀疏行很常见）
                        while cells.last().is_some_and(|c| c.is_empty()) {
                            cells.pop();
                        }
                        rows.push(cells.join("\t"));
                        in_row = false;
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if in_value || in_inline_text {
                    cell_text.push_str(t.as_ref());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if in_value || in_inline_text {
                    cell_text.push_str(&ref_text(&r));
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => {
                // 不要静默吞掉：解析中断会让后面的行凭空消失，而用户只会看到
                // 「表格少了一半」。至少在 stderr 上留下原因。
                eprintln!("工作表解析中断：{e}");
                break;
            }
        }
    }

    rows.retain(|row| !row.trim().is_empty());
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 读测试夹具。夹具由 `tools/make-office-fixtures.py` 用
    /// python-docx / openpyxl / python-pptx 生成 —— 真正能被 Office 打开的文件，
    /// 而不是我手写的 XML 片段。
    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "读不到夹具 {}（先跑 tools/make-office-fixtures.py）：{e}",
                path.display()
            )
        })
    }

    #[test]
    fn kind_of_recognises_the_supported_formats() {
        assert_eq!(kind_of("报告.docx"), Some(DocKind::Docx));
        assert_eq!(kind_of("BOOK.XLSX"), Some(DocKind::Xlsx));
        assert_eq!(kind_of("slides.pptx"), Some(DocKind::Pptx));
        // PDF 现在也走后端解析（它同样不是纯文本）
        assert_eq!(kind_of("a.pdf"), Some(DocKind::Pdf));
        assert_eq!(
            kind_of("REPORT.PDF"),
            Some(DocKind::Pdf),
            "扩展名大小写不该有影响"
        );
        assert_eq!(kind_of("noextension"), None);
        assert_eq!(kind_of("a.docx.exe"), None);
    }

    #[test]
    fn docx_extracts_paragraphs_and_table_cells() {
        let text = extract(DocKind::Docx, &fixture("sample.docx")).unwrap();
        for expected in [
            "浮语文档解析测试",
            "这是第一段，包含中文与 English mixed text。",
            "表头甲",
            "单元格二",
            "表格之后的一段。",
        ] {
            assert!(
                text.contains(expected),
                "提取结果里缺少「{expected}」：\n{text}"
            );
        }
        // 段落之间要有换行，否则所有句子会连成一坨
        assert!(text.contains('\n'), "应当保留段落换行");
    }

    #[test]
    fn xlsx_resolves_shared_strings_and_keeps_numbers() {
        let text = extract(DocKind::Xlsx, &fixture("sample.xlsx")).unwrap();
        // 这一步最容易错：单元格里的字符串存在 sharedStrings.xml 里，
        // 工作表里只留一个下标。没解这一层的话，表格会变成一串数字。
        assert!(text.contains("苹果"), "共享字符串没解出来：\n{text}");
        assert!(text.contains("香蕉"));
        assert!(text.contains("5.5"), "数字单元格丢了：\n{text}");
        assert!(text.contains("12"));
        // 两个工作表的名字都要出现
        assert!(text.contains("数据"), "工作表名丢了：\n{text}");
        assert!(text.contains("说明"));
        assert!(text.contains("第二个工作表"));
    }

    #[test]
    fn pptx_extracts_every_slide_with_page_marks() {
        let text = extract(DocKind::Pptx, &fixture("sample.pptx")).unwrap();
        assert!(text.contains("第一页标题"));
        assert!(text.contains("第二行正文"));
        assert!(text.contains("第二页标题"));
        assert!(text.contains("【幻灯片 1】"), "应当标出页码：\n{text}");
        assert!(text.contains("【幻灯片 2】"));
    }

    #[test]
    fn a_non_zip_file_is_rejected_with_a_clear_message() {
        let err = extract(DocKind::Docx, b"this is definitely not a zip file").unwrap_err();
        assert!(
            err.message.contains("不是有效的 Office 文档"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_zip_without_the_expected_part_is_rejected() {
        // 一个「空壳」zip：能打开，但没有 word/document.xml
        let mut buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(Cursor::new(&mut buf));
            writer
                .start_file::<_, ()>("hello.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            use std::io::Write;
            writer.write_all(b"hi").unwrap();
            writer.finish().unwrap();
        }
        let err = extract(DocKind::Docx, &buf).unwrap_err();
        assert!(err.message.contains("找不到正文"), "{}", err.message);
    }

    #[test]
    fn truncated_content_is_marked_not_silently_dropped() {
        // 悄悄丢内容比报错更糟：模型会以为看到的就是全部。
        let long = "啊".repeat(MAX_EXTRACTED_CHARS + 100);
        let cut = truncate_chars(long, MAX_EXTRACTED_CHARS);
        assert!(cut.chars().count() > MAX_EXTRACTED_CHARS);
        assert!(cut.contains("已截断"), "必须明确标注截断");
    }

    #[test]
    fn short_content_is_not_touched_by_the_truncator() {
        let text = "短文本".to_string();
        assert_eq!(truncate_chars(text.clone(), MAX_EXTRACTED_CHARS), text);
    }

    #[test]
    fn inline_strings_with_numeric_references_are_decoded() {
        // openpyxl 写出来的字符串是这个样子：内联（不是共享字符串表）+ 数字
        // 字符引用。第一版解析器两种都没处理好，表格里的文字整列消失 ——
        // 而数字列照常出现，看起来像「读到了但内容不对」。
        let xml = r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>&#21517;&#31216;</t></is></c><c r="B1" t="n"><v>3</v></c></row></sheetData></worksheet>"#;
        let rows = read_sheet_rows(xml, &[]);
        assert_eq!(rows, vec!["名称\t3"], "内联字符串与数字都要在：{rows:?}");
    }

    #[test]
    fn shared_string_indices_are_resolved() {
        let shared = vec!["苹果".to_string(), "香蕉".to_string()];
        let xml = r#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>1</v></c><c r="B1" t="n"><v>12</v></c></row></sheetData></worksheet>"#;
        let rows = read_sheet_rows(xml, &shared);
        assert_eq!(rows, vec!["香蕉\t12"]);
    }

    #[test]
    fn local_name_strips_the_namespace_prefix() {
        use quick_xml::name::QName;
        // quick-xml 0.42 起 QName 直接包着 &str（不再是字节）
        assert_eq!(local_name(QName("w:t")), "t");
        assert_eq!(local_name(QName("a:t")), "t");
        assert_eq!(local_name(QName("sheet")), "sheet");
        assert_eq!(local_name(QName("r:id")), "id");
    }

    #[test]
    fn slide_numbers_sort_numerically_not_lexically() {
        // 字典序会把 slide10 排在 slide2 前面 —— 幻灯片顺序就乱了
        let mut names = vec![
            "ppt/slides/slide10.xml".to_string(),
            "ppt/slides/slide2.xml".to_string(),
            "ppt/slides/slide1.xml".to_string(),
        ];
        names.sort_by_key(|n| slide_number(n));
        assert_eq!(
            names,
            vec![
                "ppt/slides/slide1.xml",
                "ppt/slides/slide2.xml",
                "ppt/slides/slide10.xml"
            ]
        );
    }
}
