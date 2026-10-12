# 生成 Office 文档测试夹具（.docx / .xlsx / .pptx）。
#
# 为什么用真实的 Office 文件而不是手写的 XML 片段：这些格式是 ZIP + 一堆 XML，
# 「我以为它是这样」和「Word 实际写出来是这样」差得很远（命名空间前缀、
# sharedStrings 的间接层、<w:t> 被拆成多个 run……）。用 python-docx /
# openpyxl / python-pptx 生成的文件，是真正能被 Office 打开的那种。
#
# 跑法：<bundled-python> tools/make-office-fixtures.py
# 产物：src-tauri/tests/fixtures/*.{docx,xlsx,pptx}（连同每份的期望文本 .txt）

import json
import os
from pathlib import Path

from docx import Document
from openpyxl import Workbook
from pptx import Presentation
from pptx.util import Inches

OUT = Path(__file__).resolve().parent.parent / "src-tauri" / "tests" / "fixtures"
OUT.mkdir(parents=True, exist_ok=True)

# ---- DOCX：多段落 + 一个表格 + 中文 ----
doc = Document()
doc.add_heading("浮语文档解析测试", level=1)
doc.add_paragraph("这是第一段，包含中文与 English mixed text。")
doc.add_paragraph("第二段有一个数字 42 和符号 $%^&。")
table = doc.add_table(rows=2, cols=2)
table.cell(0, 0).text = "表头甲"
table.cell(0, 1).text = "表头乙"
table.cell(1, 0).text = "单元格一"
table.cell(1, 1).text = "单元格二"
doc.add_paragraph("表格之后的一段。")
doc.save(OUT / "sample.docx")

# ---- XLSX：两个工作表 + 数字 / 字符串 / 公式求值结果 ----
wb = Workbook()
ws = wb.active
ws.title = "数据"
ws.append(["名称", "数量", "单价"])
ws.append(["苹果", 3, 5.5])
ws.append(["香蕉", 12, 2.25])
ws.append(["合计", 15, None])
ws2 = wb.create_sheet("说明")
ws2["A1"] = "第二个工作表"
ws2["A2"] = "中文与 数字 123"
wb.save(OUT / "sample.xlsx")

# ---- PPTX：两页幻灯片 ----
prs = Presentation()
slide = prs.slides.add_slide(prs.slide_layouts[1])
slide.shapes.title.text = "第一页标题"
slide.placeholders[1].text = "第一页正文\n第二行正文"
slide2 = prs.slides.add_slide(prs.slide_layouts[1])
slide2.shapes.title.text = "第二页标题"
slide2.placeholders[1].text = "第二页正文"
prs.save(OUT / "sample.pptx")

# ---- 期望值：让 Rust 测试断言「必须包含哪些片段」而不是逐字比对布局 ----
expect = {
    "sample.docx": [
        "浮语文档解析测试",
        "这是第一段，包含中文与 English mixed text。",
        "第二段有一个数字 42 和符号",
        "表头甲",
        "单元格二",
        "表格之后的一段。",
    ],
    "sample.xlsx": ["名称", "苹果", "香蕉", "合计", "第二个工作表", "中文与 数字 123"],
    "sample.pptx": ["第一页标题", "第一页正文", "第二行正文", "第二页标题", "第二页正文"],
}
(OUT / "expect.json").write_text(
    json.dumps(expect, ensure_ascii=False, indent=2), encoding="utf-8"
)

for p in sorted(OUT.iterdir()):
    print(f"  {p.name}  {p.stat().st_size} 字节")
print("夹具已生成")
