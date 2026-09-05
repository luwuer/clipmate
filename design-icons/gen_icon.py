#!/usr/bin/env python3
"""Clipmate 图标生成器：WorkBuddy 风格绿渐变 + 白色 C + 三根白杠"""
from PIL import Image, ImageDraw

S = 4  # 超采样倍数
BASE = 1024
BIG = BASE * S  # 4096

# ---- 三段渐变色（WorkBuddy 渐变规则，绿色→红色系：玫红→正红→珊瑚粉）----
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


# ---- 背景：1D 对角渐变条 → 拉伸成方形（(x+y) 归一化即对角渐变）----
strip = Image.new("RGB", (1, BIG))
px = strip.load()
for i in range(BIG):
    px[0, i] = grad_color(i / (BIG - 1))
bg = strip.resize((BIG, BIG))

# ---- 圆角 squircle mask ----
mask = Image.new("L", (BIG, BIG), 0)
md = ImageDraw.Draw(mask)
RADIUS = int(229 * S)  # macOS squircle ≈ 22.37%
md.rounded_rectangle([0, 0, BIG - 1, BIG - 1], radius=RADIUS, fill=255)
bg.putalpha(mask)

layer = Image.new("RGBA", (BIG, BIG), (0, 0, 0, 0))
draw = ImageDraw.Draw(layer)

# ---- 白色 C：粗弧 + 圆头端点 ----
CX = CY = BIG // 2
R = 260 * S            # 弧中心线半径
STROKE = 110 * S       # 线宽
OPEN_HALF = 55         # 开口半角（度）
import math

bbox = [CX - R - STROKE // 2, CY - R - STROKE // 2, CX + R + STROKE // 2, CY + R + STROKE // 2]
# PIL 角度：0°=3点钟，顺时针增。开口朝右 → 弧从 55°(右下) 顺时针到 305°(右上)
draw.arc(bbox, start=OPEN_HALF, end=360 - OPEN_HALF, fill=WHITE, width=STROKE)
# 圆头端点
cap_r = STROKE // 2
for ang in (OPEN_HALF, 360 - OPEN_HALF):
    a = math.radians(ang)
    ex, ey = CX + R * math.cos(a), CY + R * math.sin(a)
    draw.ellipse([ex - cap_r, ey - cap_r, ex + cap_r, ey + cap_r], fill=WHITE)

# ---- 三根白色圆角杠（居中于 C 圆心 512,512，长短递减不一致）----
BAR_H = 34
CX_BAR = 512
BAR_WIDTHS = [112, 150, 82]  # 从上到下：中、长、短
for yc, bw in zip((445, 512, 579), BAR_WIDTHS):
    y1, y2 = yc - BAR_H // 2, yc + BAR_H // 2
    x1, x2 = CX_BAR - bw // 2, CX_BAR + bw // 2
    draw.rounded_rectangle(
        [x1 * S, y1 * S, x2 * S, y2 * S],
        radius=(BAR_H // 2) * S, fill=WHITE,
    )

# ---- 合成 & 缩小（超采样即抗锯齿）----
out = Image.new("RGBA", (BIG, BIG), (0, 0, 0, 0))
out.alpha_composite(bg)
out.alpha_composite(layer)
final = out.resize((BASE, BASE), Image.LANCZOS)
final.save("/Users/mdy/workspace_test/repos/clipmate/design-icons/icon-v9.png")
print("saved icon-v9.png", final.size)
