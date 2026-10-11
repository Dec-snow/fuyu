# 读截图里的探针像素条，报告每一项渲染检查的结果。
# 探针把结果编码成 24x24 的方块（绿=通过 / 红=失败），这里逐个读回来。
import sys
from PIL import Image

CHECKS = [
    "KaTeX 渲染出结构",
    "块级公式（$$…$$）是 display 模式",
    "KaTeX 的 CSS/字体生效",
    "代码高亮产生了 token",
    "带语言的代码块走了高亮分支",
    "没有公式走到降级分支",
    "价格 $5/$10 没被误判成公式",
    "深色主题下同样有高亮",
]

# 高亮配色（light 主题的 --syntax-* 值）：截图里应当能找到这些精确颜色。
#
# 注意：这几种 token 必须**在样例代码里真的出现**才会命中。
# 第一次跑时 number 是 0 命中 —— 不是产品问题，是样例里根本没有数字字面量。
# 所以样例代码刻意带上了 关键字/字符串/数字/注释 四种 token。
SYNTAX_COLORS = {
    "keyword #7c3aed": (0x7C, 0x3A, 0xED),
    "string #0f766e": (0x0F, 0x76, 0x6E),
    "number #b45309": (0xB4, 0x53, 0x09),
    "meta #6b7280": (0x6B, 0x72, 0x80),
}


def near(pixel, target, tol=12):
    return all(abs(int(a) - int(b)) <= tol for a, b in zip(pixel[:3], target))


def main(path: str) -> int:
    img = Image.open(path).convert("RGB")
    print(f"截图 = {path}  尺寸 = {img.width}x{img.height}\n")

    print("=== 渲染检查（探针像素条，绿=通过 / 红=失败）===")
    failed = 0
    for i, name in enumerate(CHECKS):
        x = i * 24 + 12  # 每个方块 24px，取中心
        y = 12
        px = img.getpixel((x, y))
        ok = near(px, (0x00, 0xFF, 0x00))
        bad = near(px, (0xFF, 0x00, 0x00))
        mark = "✓" if ok else ("✗" if bad else "?")
        if not ok:
            failed += 1
        print(f"  {mark} {name}   (像素 {px})")

    print("\n=== 代码高亮配色是否真的出现在画面上 ===")
    pix = list(img.getdata())
    for label, target in SYNTAX_COLORS.items():
        count = sum(1 for p in pix if near(p, target))
        mark = "✓" if count > 0 else "✗"
        if count == 0:
            failed += 1
        print(f"  {mark} {label}  命中 {count} 个像素")

    # 反证：整张图里出现的颜色数（纯文本渲染会明显更少）
    unique = len(set(pix))
    print(f"\n  整图不同颜色数 = {unique}")

    print()
    if failed == 0:
        print("全部通过")
        return 0
    print(f"有 {failed} 项未通过")
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else "dist/preview.png"))
