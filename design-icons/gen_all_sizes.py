#!/usr/bin/env python3
"""从 icon-v7.png 生成 Tauri 全套图标资产（替代 npx tauri icon）"""
import os
import re
import shutil
import subprocess
from PIL import Image, ImageDraw

SRC = "/Users/mdy/workspace_test/repos/clipmate/app/icons/icon.png"  # 已是 v7 1024x1024
ICONS = "/Users/mdy/workspace_test/repos/clipmate/app/icons"

src = Image.open(SRC).convert("RGBA")


def save_resized(size, path):
    img = src.resize((size, size), Image.LANCZOS)
    img.save(path)


# ---- 1. 桌面 PNG 各尺寸 ----
plain = {"32x32.png": 32, "64x64.png": 64, "128x128.png": 128, "128x128@2x.png": 256}
for name, size in plain.items():
    save_resized(size, os.path.join(ICONS, name))

# Windows Store logos（Square{N}x{N}Logo.png / StoreLogo.png=50）
for f in os.listdir(ICONS):
    m = re.match(r"Square(\d+)x\d+Logo\.png$", f)
    if m:
        save_resized(int(m.group(1)), os.path.join(ICONS, f))
save_resized(50, os.path.join(ICONS, "StoreLogo.png"))

# ---- 2. icon.icns（iconset → iconutil）----
iconset = os.path.join(ICONS, "icon.iconset")
if os.path.exists(iconset):
    shutil.rmtree(iconset)
os.makedirs(iconset)
icns_sizes = [16, 32, 128, 256, 512]
for s in icns_sizes:
    save_resized(s, os.path.join(iconset, f"icon_{s}x{s}.png"))
    save_resized(s * 2, os.path.join(iconset, f"icon_{s}x{s}@2x.png"))
subprocess.run(
    ["iconutil", "-c", "icns", "-o", os.path.join(ICONS, "icon.icns"), iconset],
    check=True,
)
shutil.rmtree(iconset)

# ---- 3. icon.ico（PIL 多尺寸）----
src.resize((256, 256), Image.LANCZOS).save(
    os.path.join(ICONS, "icon.ico"),
    sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
)

# ---- 4. iOS AppIcon（按文件名解析尺寸）----
ios_dir = os.path.join(ICONS, "ios")
for f in os.listdir(ios_dir):
    m = re.match(r"AppIcon-(\d+(?:\.\d+)?)(?:@\d+)?(?:-\d+)?x\.png$", f)
    if m:
        base = float(m.group(1))
        # 从文件名提取 scale
        sm = re.search(r"@(\d+)x", f)
        scale = int(sm.group(1)) if sm else 1
        size = int(base * scale)
        save_resized(size, os.path.join(ios_dir, f))

# ---- 5. Android mipmap ----
densities = {"mdpi": 48, "hdpi": 72, "xhdpi": 96, "xxhdpi": 144, "xxxhdpi": 192}
fg_sizes = {"mdpi": 108, "hdpi": 162, "xhdpi": 216, "xxhdpi": 324, "xxxhdpi": 432}
for d, size in densities.items():
    ddir = os.path.join(ICONS, "android", f"mipmap-{d}")
    save_resized(size, os.path.join(ddir, "ic_launcher.png"))
    # 圆形版：圆形 mask 裁剪
    img = src.resize((size, size), Image.LANCZOS)
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).ellipse([0, 0, size - 1, size - 1], fill=255)
    img.putalpha(mask)
    img.save(os.path.join(ddir, "ic_launcher_round.png"))
    # 前景层
    save_resized(fg_sizes[d], os.path.join(ddir, "ic_launcher_foreground.png"))

print("ALL DONE")
