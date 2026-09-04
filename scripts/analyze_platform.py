#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
分析日志中 platform 占比及各 platform 下 version 占比，生成 HTML 报告。

用法:
    python3 analyze_platform.py <日志1> [日志2 ...] [-o 输出HTML路径]

多个日志会合并统计（设备数按 deviceid 去重）。
"""

import datetime
import html
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

ANSI_RE = re.compile(r'(?:\x1b|\u001b)?\[[0-9;]*m')
PLATFORM_RE = re.compile(r'[?&]platform=([^&\s]+)')
VERSION_RE = re.compile(r'[?&]version=([^&\s]+)')
DEVICE_RE = re.compile(r'[?&]deviceid=([^&\s]+)')

UNKNOWN = '(未知)'


def esc(s):
    return html.escape(str(s), quote=True)


def pct(n, total):
    return (n / total * 100) if total else 0.0


def parse_log(path):
    platform_counter = Counter()                       # platform -> 请求数
    platform_version = defaultdict(Counter)            # platform -> version -> 请求数
    platform_devices = defaultdict(set)                # platform -> set(deviceid)
    platform_version_devices = defaultdict(lambda: defaultdict(set))
    total_lines = 0
    matched = 0

    with open(path, encoding='utf-8', errors='replace') as f:
        for line in f:
            total_lines += 1
            clean = ANSI_RE.sub('', line)
            pm = PLATFORM_RE.search(clean)
            vm = VERSION_RE.search(clean)
            dm = DEVICE_RE.search(clean)
            if not pm:
                continue
            matched += 1
            platform = pm.group(1) or UNKNOWN
            version = vm.group(1) if vm else UNKNOWN
            device = dm.group(1) if dm else None

            platform_counter[platform] += 1
            platform_version[platform][version] += 1
            if device:
                platform_devices[platform].add(device)
                platform_version_devices[platform][version].add(device)

    return {
        'total_lines': total_lines,
        'matched': matched,
        'platform_counter': platform_counter,
        'platform_version': platform_version,
        'platform_devices': platform_devices,
        'platform_version_devices': platform_version_devices,
    }


PLATFORM_COLORS = [
    '#4f7cff', '#00b96b', '#ff8800', '#eb2f96', '#722ed1', '#13c2c2', '#faad14',
]


def bar_html(ratio, color):
    return (
        f'<div class="bar-wrap"><div class="bar" style="width:{min(ratio * 100, 100):.2f}%;'
        f'background:{color};"></div></div>'
    )


def build_report(log_paths, data):
    total = data['matched']
    platforms = sorted(data['platform_counter'].items(), key=lambda x: x[1], reverse=True)
    colors = {p: PLATFORM_COLORS[i % len(PLATFORM_COLORS)] for i, (p, _) in enumerate(platforms)}

    names = '、'.join(Path(p).name for p in log_paths)
    merge_note = f'（{len(log_paths)} 份日志合并统计）' if len(log_paths) > 1 else ''
    now = datetime.datetime.now().strftime('%Y-%m-%d %H:%M:%S')
    total_devices = len(set().union(*data['platform_devices'].values())) if data['platform_devices'] else 0

    # ---- 概览卡片 ----
    cards = f'''
    <div class="cards">
      <div class="card"><div class="card-num">{total:,}</div><div class="card-label">有效请求数</div></div>
      <div class="card"><div class="card-num">{len(platforms)}</div><div class="card-label">Platform 数</div></div>
      <div class="card"><div class="card-num">{total_devices:,}</div><div class="card-label">独立设备数</div></div>
      <div class="card"><div class="card-num">{data["total_lines"]:,}</div><div class="card-label">日志总行数</div></div>
    </div>'''

    # ---- Platform 占比表 ----
    rows = []
    for p, cnt in platforms:
        ratio = pct(cnt, total)
        ver_cnt = len(data['platform_version'][p])
        dev_cnt = len(data['platform_devices'][p])
        rows.append(f'''
        <tr>
          <td><span class="dot" style="background:{colors[p]}"></span><b>{esc(p)}</b></td>
          <td class="num">{cnt:,}</td>
          <td class="num"><b>{ratio:.2f}%</b></td>
          <td>{bar_html(ratio / 100, colors[p])}</td>
          <td class="num">{dev_cnt:,}</td>
          <td class="num">{ver_cnt}</td>
        </tr>''')
    platform_table = f'''
    <h2>Platform 占比 <span class="sub">共 {total:,} 条请求</span></h2>
    <table>
      <thead><tr><th>Platform</th><th>请求数</th><th>占比</th><th style="width:32%">分布</th><th>独立设备</th><th>版本数</th></tr></thead>
      <tbody>{''.join(rows)}</tbody>
    </table>'''

    # ---- 各 Platform 的 Version 占比 ----
    sections = []
    for p, cnt in platforms:
        versions = sorted(data['platform_version'][p].items(), key=lambda x: x[1], reverse=True)
        vrows = []
        for v, vcnt in versions:
            vratio = pct(vcnt, cnt)
            vdev = len(data['platform_version_devices'][p][v])
            vrows.append(f'''
          <tr>
            <td><code>{esc(v)}</code></td>
            <td class="num">{vcnt:,}</td>
            <td class="num"><b>{vratio:.2f}%</b></td>
            <td>{bar_html(vratio / 100, colors[p])}</td>
            <td class="num">{vdev:,}</td>
          </tr>''')
        sections.append(f'''
    <section class="ver-section">
      <h2><span class="dot" style="background:{colors[p]}"></span>{esc(p)}
        <span class="sub">{cnt:,} 条请求 · {len(versions)} 个版本 · 占全部请求 {pct(cnt, total):.2f}%</span>
      </h2>
      <table>
        <thead><tr><th>Version</th><th>请求数</th><th>平台内占比</th><th style="width:38%">分布</th><th>独立设备</th></tr></thead>
        <tbody>{''.join(vrows)}</tbody>
      </table>
    </section>''')

    return f'''<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Platform / Version 占比报告</title>
<style>
  * {{ box-sizing: border-box; }}
  body {{ margin:0; font-family: -apple-system, "PingFang SC", "Segoe UI", "Microsoft YaHei", sans-serif;
         background:#f5f7fb; color:#1f2329; }}
  .container {{ max-width: 1080px; margin: 0 auto; padding: 32px 24px 60px; }}
  header {{ background: linear-gradient(135deg,#4f7cff 0%,#722ed1 100%); color:#fff;
           border-radius: 16px; padding: 28px 32px; margin-bottom: 24px; }}
  header h1 {{ margin:0 0 8px; font-size: 24px; }}
  header .meta {{ font-size: 13px; opacity:.9; line-height:1.8; }}
  .cards {{ display:grid; grid-template-columns:repeat(auto-fit,minmax(180px,1fr)); gap:16px; margin-bottom:28px; }}
  .card {{ background:#fff; border-radius:14px; padding:20px; box-shadow:0 1px 4px rgba(31,35,41,.06);
          text-align:center; }}
  .card-num {{ font-size:28px; font-weight:700; color:#4f7cff; }}
  .card-label {{ margin-top:6px; font-size:13px; color:#646a73; }}
  h2 {{ font-size:18px; margin:28px 0 12px; }}
  h2 .sub {{ font-size:13px; font-weight:400; color:#8a919f; margin-left:10px; }}
  table {{ width:100%; border-collapse:collapse; background:#fff; border-radius:12px; overflow:hidden;
          box-shadow:0 1px 4px rgba(31,35,41,.06); font-size:14px; }}
  th, td {{ padding:10px 14px; text-align:left; border-bottom:1px solid #f0f2f5; }}
  th {{ background:#fafbfc; color:#646a73; font-weight:600; font-size:13px; }}
  tbody tr:hover {{ background:#f7f9ff; }}
  tbody tr:last-child td {{ border-bottom:none; }}
  td.num {{ font-variant-numeric: tabular-nums; white-space:nowrap; }}
  .dot {{ display:inline-block; width:10px; height:10px; border-radius:3px; margin-right:8px;
         vertical-align:1px; }}
  code {{ background:#f2f4f7; padding:2px 8px; border-radius:6px; font-size:13px; }}
  .bar-wrap {{ background:#eef1f6; border-radius:99px; height:10px; width:100%; min-width:120px; overflow:hidden; }}
  .bar {{ height:100%; border-radius:99px; }}
  footer {{ margin-top:40px; font-size:12px; color:#8a919f; text-align:center; }}
</style>
</head>
<body>
<div class="container">
  <header>
    <h1>Platform / Version 占比报告</h1>
    <div class="meta">
      日志文件：{esc(names)}{merge_note}<br>
      生成时间：{now} · 有效请求 {total:,} / 总行数 {data['total_lines']:,}
    </div>
  </header>
  {cards}
  {platform_table}
  {''.join(sections)}
  <footer>由 analyze_platform.py 自动生成</footer>
</div>
</body>
</html>
'''


def merge_logs(paths):
    """合并多份日志的统计（设备数按 deviceid 去重）"""
    merged = {
        'total_lines': 0,
        'matched': 0,
        'platform_counter': Counter(),
        'platform_version': defaultdict(Counter),
        'platform_devices': defaultdict(set),
        'platform_version_devices': defaultdict(lambda: defaultdict(set)),
    }
    for path in paths:
        d = parse_log(path)
        merged['total_lines'] += d['total_lines']
        merged['matched'] += d['matched']
        merged['platform_counter'].update(d['platform_counter'])
        for p, vc in d['platform_version'].items():
            merged['platform_version'][p].update(vc)
        for p, s in d['platform_devices'].items():
            merged['platform_devices'][p] |= s
        for p, vd in d['platform_version_devices'].items():
            for v, s in vd.items():
                merged['platform_version_devices'][p][v] |= s
    return merged


def main():
    import argparse
    parser = argparse.ArgumentParser(description='分析日志 platform/version 占比并生成 HTML 报告')
    parser.add_argument('logs', nargs='+', help='一个或多个日志文件（多个时合并统计）')
    parser.add_argument('-o', '--output', help='输出 HTML 路径（默认按日志名生成）')
    args = parser.parse_args()

    if args.output:
        out_path = args.output
    elif len(args.logs) == 1:
        out_path = str(Path(args.logs[0]).with_suffix('')) + '-platform-report.html'
    else:
        out_path = str(Path(args.logs[0]).parent / 'merged-platform-report.html')

    data = merge_logs(args.logs)
    if data['matched'] == 0:
        print('未在日志中找到 platform= 字段，请确认日志格式')
        sys.exit(2)

    report = build_report(args.logs, data)
    Path(out_path).write_text(report, encoding='utf-8')

    # 控制台同步输出摘要
    total = data['matched']
    all_devices = len(set().union(*data['platform_devices'].values())) if data['platform_devices'] else 0
    print(f'有效请求: {total} / 总行数: {data["total_lines"]} / 独立设备(去重): {all_devices:,}')
    print(f'{"Platform":<12}{"请求数":>10}{"占比":>10}   设备数')
    for p, c in sorted(data['platform_counter'].items(), key=lambda x: x[1], reverse=True):
        print(f'{p:<12}{c:>10,}{pct(c, total):>9.2f}%   {len(data["platform_devices"][p]):,}')
        for v, vc in sorted(data['platform_version'][p].items(), key=lambda x: x[1], reverse=True):
            print(f'  {"":<10}{v:<14}{vc:>6,}{pct(vc, c):>9.2f}%')
    print(f'\nHTML 报告已生成: {out_path}')


if __name__ == '__main__':
    main()
