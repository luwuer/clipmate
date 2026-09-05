#!/usr/bin/env python3
"""Clipmate 应用图标生成器（v11 定稿几何，勿改回满铺！）

⚠️ 图标标准（2026-09-05 实测 Safari/Finder/WorkBuddy 官方图标得出）：
  - 画布 1024×1024，内容 squircle 820×820，位于 (102,102)
  - 四周留 102px 透明边距，四角 alpha=0（圆角烧在 PNG 里）
  - 圆角半径 180（≈内容区 21.8%，macOS squircle 标准 ~22.37%）
  - ❌ 禁止满铺 100% 画布（会导致 Launchpad 里图标比其他应用大）
  - ❌ 禁止用 npx tauri icon 从满铺源图重新生成
  - 改动后必须：python3 gen_icon.py && cp icon-final.png ../app/icons/icon.png
    && python3 gen_all_sizes.py && 提交 git（未提交会被 git 还原！）

设计：WorkBuddy 渐变规则（左上中明度/中间最深/右下最亮）的红色系
     + 白色几何 C + C 圆心居中三根白杠（150/112/82 递减，剪贴板条目）
"""
import math
from PIL import Image, ImageDraw

S = 4  # 超采样倍数（4096 绘制 → LANCZOS 缩至 1024，抗锯齿）
BASE = 1024
BIG = BASE * S  # 4096

# ---- v11 几何（官方图标实测对齐）----
CONTENT = 820          # 内容 squircle 边长
MARGIN = (BASE - CONTENT) // 2   # 102 透明边距
RADIUS = 180           # 圆角半径（内容区的 ~21.8%）

# ---- 三段渐变色（WorkBuddy 渐变规则换红色系）----
C1 = (0xF4, 0x3F, 0x5E)   # 左上 玫红 rose-500
C2 = (0xDC, 0x26, 0x26)   # 中间 正红 red-600（最饱和最深）
C3 = (0xFB, 0x71, 0x85)   # 右下 珊瑚粉 rose-400（最亮）
WHITE = (255, 255, 255, 255)


def grad_color(t):
    """t∈[0,1] 沿对角线的三段线性插值"""
    if t < 0.5:
        k = t / 0.5
        a, b = C1, C2
    else:
        k = (t - 0.5) / 0.5
        a, b = C2, C3
    return tuple(int(a[i] + (b[i] - a[i]) * k) for i in range(3))


# ---- 背景：1D 对角渐变条 → 拉伸成方形 ----
strip = Image.new("RGB", (1, BIG))
px = strip.load()
for i in range(BIG):
    px[0, i] = grad_color(i / (BIG - 1))
bg = strip.resize((BIG, BIG))

# ---- 圆角 squircle mask：只覆盖内容区 820@102，四周留透明 ----
mask = Image.new("L", (BIG, BIG), 0)
md = ImageDraw.Draw(mask)
x0, y0 = MARGIN * S, MARGIN * S
x1, y1 = (MARGIN + CONTENT) * S, (MARGIN + CONTENT) * S
md.rounded_rectangle([x0, y0, x1 - 1, y1 - 1], radius=RADIUS * S, fill=255)
bg.putalpha(mask)

layer = Image.new("RGBA", (BIG, BIG), (0, 0, 0, 0))
draw = ImageDraw.Draw(layer)

# ---- 白色 C：粗弧 + 圆头端点（圆心=画布中心 512 = 内容区中心）----
CX = CY = BIG // 2
R = 260 * S            # 弧中心线半径
STROKE = 110 * S       # 线宽
OPEN_HALF = 55         # 开口半角（度）

bbox = [CX - R - STROKE // 2, CY - R - STROKE // 2, CX + R + STROKE // 2, CY + R + STROKE // 2]
draw.arc(bbox, start=OPEN_HALF, end=360 - OPEN_HALF, fill=WHITE, width=STROKE)
cap_r = STROKE // 2
for ang in (OPEN_HALF, 360 - OPEN_HALF):
    a = math.radians(ang)
    ex, ey = CX + R * math.cos(a), CY + R * math.sin(a)
    draw.ellipse([ex - cap_r, ey - cap_r, ex + cap_r, ey + cap_r], fill=WHITE)

# ---- 三根白色圆角杠（居中于 C 圆心，从上到下递减：150/112/82）----
BAR_H = 34
BAR_WIDTHS = [150, 112, 82]
for yc, bw in zip((445, 512, 579), BAR_WIDTHS):
    y1, y2 = yc - BAR_H // 2, yc + BAR_H // 2
    x1, x2 = 512 - bw // 2, 512 + bw // 2
    draw.rounded_rectangle(
        [x1 * S, y1 * S, x2 * S, y2 * S],
        radius=(BAR_H // 2) * S, fill=WHITE,
    )

# ---- 合成 & 缩小 ----
out = Image.new("RGBA", (BIG, BIG), (0, 0, 0, 0))
out.alpha_composite(bg)
out.alpha_composite(layer)
final = out.resize((BASE, BASE), Image.LANCZOS)
OUT = "/Users/mdy/workspace_test/repos/clipmate/design-icons/icon-final.png"
final.save(OUT)
print("saved", OUT, final.size)
