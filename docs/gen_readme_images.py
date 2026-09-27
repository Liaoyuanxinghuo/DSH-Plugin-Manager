# -*- coding: utf-8 -*-
"""README 信息图生成：界面布局 / 核心概念 / 功能总览"""
import os
from PIL import Image, ImageDraw, ImageFont

OUT = r"E:\workspace\dshcjaz\docs\images"
os.makedirs(OUT, exist_ok=True)
FONT = r"C:\Windows\Fonts\msyh.ttc"
FONT_BOLD = r"C:\Windows\Fonts\msyhbd.ttc"

ACCENT = (31, 111, 217)      # 蓝
GREEN = (22, 163, 74)        # 绿
ORANGE = (217, 158, 49)      # 橙
GRAY_BG = (246, 248, 251)
CARD = (255, 255, 255)
BORDER = (214, 222, 232)
DARK = (38, 46, 58)
GRAY = (110, 120, 134)

def font(sz, bold=False):
    p = FONT_BOLD if bold else FONT
    try:
        return ImageFont.truetype(p, sz)
    except OSError:
        return ImageFont.truetype(FONT, sz)

def text_w(draw, s, f):
    b = draw.textbbox((0, 0), s, font=f)
    return b[2] - b[0]

def center(draw, cx, y, s, f, fill=DARK):
    draw.text((cx - text_w(draw, s, f) / 2, y), s, font=f, fill=fill)

def wrap(draw, s, f, maxw):
    out, line = [], ""
    for ch in s:
        if text_w(draw, line + ch, f) > maxw:
            out.append(line); line = ch
        else:
            line += ch
    if line: out.append(line)
    return out

def rrect(d, box, r, fill=None, outline=None, width=1):
    d.rounded_rectangle(box, radius=r, fill=fill, outline=outline, width=width)

def new_canvas(w=1280, h=720, title=None):
    img = Image.new("RGB", (w, h), GRAY_BG)
    d = ImageDraw.Draw(img)
    if title:
        d.text((40, 28), title, font=font(34, True), fill=DARK)
        d.line([(40, 78), (w - 40, 78)], fill=BORDER, width=2)
    return img, d

def save(img, name):
    p = os.path.join(OUT, name)
    img.save(p)
    print("saved", p)

# ============ 图 1：界面布局 ============
img, d = new_canvas(title="DSH Plugin Manager — 三栏界面布局")
# 窗口外框
rrect(d, (40, 110, 1240, 680), 16, fill=CARD, outline=BORDER, width=2)
d.text((64, 128), "DSH Plugin Manager", font=font(22, True), fill=DARK)
d.text((1080, 132), "— ×", font=font(16), fill=GRAY)
# 标题栏按钮
for i, (x, t) in enumerate([(300, "下载 DSH"), (420, "扫本体"), (540, "?"), (760, "新建"), (880, "扫Profile"), (1000, "导入")]):
    rrect(d, (x, 122, x + 86, 152), 8, fill=(241, 244, 248), outline=BORDER)
    center(d, x + 43, 127, t, font(13), DARK)

def panel(x0, y0, x1, y1, head, items, active_idx, accent):
    rrect(d, (x0, y0, x1, y1), 12, fill=(250, 251, 253), outline=BORDER)
    d.text((x0 + 18, y0 + 14), head, font=font(18, True), fill=DARK)
    d.text((x1 - 60, y0 + 16), "?", font=font(15, True), fill=GRAY)
    yy = y0 + 52
    for i, it in enumerate(items):
        c = accent if i == active_idx else CARD
        rrect(d, (x0 + 14, yy, x1 - 14, yy + 62), 8, fill=c, outline=(accent if i == active_idx else BORDER), width=(2 if i == active_idx else 1))
        d.ellipse((x0 + 28, yy + 22, x0 + 40, yy + 34), fill=accent)
        d.text((x0 + 50, yy + 10), it[0], font=font(15, True), fill=DARK)
        d.text((x0 + 50, yy + 33), it[1], font=font(12), fill=GRAY)
        yy += 74

# 左栏：DSH 环境
panel(58, 180, 380, 660, "DSH 环境", [
    ("Global CLI", "dsh 0.1.0-rc.7 · 全局"),
    ("0.1.7-rc.2-mc", "dsh 0.1.7-rc.2 · 扫描目录"),
    ("0.2.0-rc.1", "dsh 0.2.0-rc.1 · 下载安装"),
], 1, ACCENT)
# 中栏：Profiles
panel(396, 180, 720, 660, "Profiles", [
    ("web", "6 插件 · 12.4 MB · ~/.dsh/profiles"),
    ("harness-p", "3 插件 · 3.1 MB · .dsh-packs/.../profiles"),
    ("dev", "0 插件 · 1.2 MB · ~/.dsh/profiles"),
], 0, GREEN)
# 右栏：运行控制 + 插件
rrect(d, (736, 180, 1222, 660), 12, fill=(250, 251, 253), outline=BORDER)
d.text((754, 194), "运行控制  ·  web @ 0.1.7-rc.2-mc", font=font(16, True), fill=DARK)
rrect(d, (754, 234, 1204, 300), 8, fill=CARD, outline=BORDER)
d.text((772, 246), "启动 DSH    停止    重启    打开界面", font=font(14), fill=DARK)
d.text((772, 274), "运行中 PID 19476 · 端口 3080", font=font(12), fill=GRAY)
d.text((754, 320), "插件列表", font=font(15, True), fill=DARK)
for i, (n, v) in enumerate([("@deepseek-ai/cordis", "4.0.1"), ("dsh-market", "1.66.1"), ("assistant-regenerate", "0.1.0")]):
    rrect(d, (754, 344 + i * 52, 1204, 388 + i * 52), 8, fill=CARD, outline=BORDER)
    d.text((772, 354 + i * 52), n, font=font(13, True), fill=DARK)
    d.text((1080, 354 + i * 52), v, font=font(12), fill=GRAY)
    rrect(d, (1160, 352 + i * 52, 1192, 368 + i * 52), 10, fill=GREEN)
    d.ellipse((1184, 356 + i * 52, 1188, 360 + i * 52), fill=(255, 255, 255))
d.text((754, 512), "安装插件    导出    删除", font=font(13), fill=GRAY)
save(img, "layout.png")

# ============ 图 2：核心概念（组合启动） ============
img, d = new_canvas(title="核心概念 — 任意 DSH 版本 × 任意 Profile，独立进程运行")
# 左：DSH 环境
rrect(d, (40, 130, 380, 420), 14, fill=CARD, outline=BORDER)
d.text((60, 150), "DSH 环境（左栏）", font=font(17, True), fill=DARK)
envs = [("Global CLI", "0.1.0-rc.7"), ("0.1.7-rc.2-mc", "0.1.7-rc.2"), ("0.2.0-rc.1", "0.2.0-rc.1")]
for i, (n, v) in enumerate(envs):
    rrect(d, (60, 190 + i * 70, 360, 250 + i * 70), 8, fill=(241, 244, 248), outline=BORDER)
    d.text((78, 202 + i * 70), n, font=font(14, True), fill=DARK)
    d.text((78, 224 + i * 70), v, font=font(12), fill=GRAY)
# 右：Profiles
rrect(d, (900, 130, 1240, 420), 14, fill=CARD, outline=BORDER)
d.text((920, 150), "Profiles（中栏）", font=font(17, True), fill=DARK)
profs = [("web", "~/.dsh/profiles"), ("harness-p", ".dsh-packs/.../profiles"), ("dev", "~/.dsh/profiles")]
for i, (n, v) in enumerate(profs):
    rrect(d, (920, 190 + i * 70, 1220, 250 + i * 70), 8, fill=(241, 244, 248), outline=BORDER)
    d.text((938, 202 + i * 70), n, font=font(14, True), fill=DARK)
    d.text((938, 224 + i * 70), v, font=font(12), fill=GRAY)
# 中间：× 与 组合结果
center(d, 640, 240, "×", font(36, True), ORANGE)
center(d, 640, 300, "任意组合", font(15), GRAY)
# 底部：结果卡片
rrect(d, (40, 470, 1240, 660), 14, fill=CARD, outline=BORDER)
d.text((60, 486), "每个组合 = 一个独立进程", font=font(18, True), fill=DARK)
cards = [
    ("web × Global CLI", "PID 1001 · 端口 3080"),
    ("harness-p × 0.1.7-rc.2-mc", "PID 1002 · 端口 3081"),
    ("dev × 0.2.0-rc.1", "PID 1003 · 端口 3082"),
]
for i, (t, s) in enumerate(cards):
    x0 = 60 + i * 405
    rrect(d, (x0, 540, x0 + 370, 620), 10, fill=(241, 244, 248), outline=BORDER)
    d.text((x0 + 20, 556), t, font=font(14, True), fill=DARK)
    d.text((x0 + 20, 582), s, font=font(12), fill=GRAY)
d.text((60, 636), "启动时自动注入 DSH_HOME = Profile 来源目录的父目录；「打开界面」实时提取带 token 的认证地址", font=font(12), fill=GRAY)
save(img, "concept.png")

# ============ 图 3：功能总览 ============
img, d = new_canvas(title="功能总览")
feats = [
    ("插件管理", ACCENT, [
        "在线：市场 / NPM 搜索 / GitHub / 自定义源",
        "本地：文件夹 或 .tgz 压缩包（file: 协议）",
        "卸载 · 更新 · 拉杆启停 · 版本豁免",
    ]),
    ("Profile 管理", GREEN, [
        "新建 · 删除 · 导入 / 导出 zip（重名自动改名）",
        "扫描本地 profiles 目录，合并展示",
        "每个 Profile 标注来源目录",
    ]),
    ("DSH 环境", ORANGE, [
        "下载任意版本到指定目录，多版本共存",
        "扫描本体：全局 npm / DSH Desktop / 本地项目",
        "镜像源设置，解决国内下载失败",
    ]),
    ("多实例运行", (66, 84, 128), [
        "任意 DSH × 任意 Profile 独立进程",
        "端口自动分配（3080 起）互不冲突",
        "独立启动 / 停止 / 重启，进程自动清理",
    ]),
]
for i, (title, accent, items) in enumerate(feats):
    x0 = 40 + (i % 2) * 610
    y0 = 130 + (i // 2) * 265
    rrect(d, (x0, y0, x0 + 590, y0 + 245), 14, fill=CARD, outline=BORDER)
    rrect(d, (x0, y0, x0 + 590, y0 + 58), 14, fill=accent)
    d.rectangle((x0, y0 + 30, x0 + 590, y0 + 58), fill=accent)
    d.text((x0 + 20, y0 + 14), title, font=font(19, True), fill=(255, 255, 255))
    yy = y0 + 80
    for it in items:
        d.ellipse((x0 + 24, yy + 5, x0 + 34, yy + 15), fill=accent)
        for ln in wrap(d, it, font(14), 520):
            d.text((x0 + 44, yy), ln, font=font(14), fill=DARK)
            yy += 26
        yy += 10
save(img, "features.png")

print("ALL DONE")
