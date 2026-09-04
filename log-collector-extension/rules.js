/**
 * 解析规则引擎：多套规则适配不同日志格式。
 *
 * 规则为纯 JSON（可直接存储/序列化传输），由引擎编译为正则执行：
 *   - lineMatch:   行过滤子串，包含该子串的行才会做字段提取（空 = 所有行）
 *   - fields:      字段名 -> 正则（第 1 个捕获组为字段值；无捕获组则取整段匹配）
 *   - dimensions:  统计维度 [主维度, 次维度(可选)]，均为字段名
 *   - deviceField: 设备字段（可选），用于统计独立设备数
 *   - dedupeBy:    去重字段（可选），如 callid
 *   - truncation:  截断标记正则（可选），命中则判定结果被截断
 *
 * collector 页与 content script 共用本文件。
 */
const LogRules = (() => {
  'use strict';

  const BUILTIN_RULES = [
    {
      id: 'builtin-platform-version',
      name: '平台和版本统计',
      builtin: true,
      description: '提取请求 URL 中的 platform/version/deviceid，统计平台占比及各平台下版本占比',
      lineMatch: 'platform=',
      fields: {
        ts: '\\d{4}-\\d{2}-\\d{2} \\d{2}:\\d{2}:\\d{2}',
        platform: '[?&]platform=([^&\\s]+)',
        version: '[?&]version=([^&\\s]+)',
        deviceid: '[?&]deviceid=([^&\\s]+)',
        callid: 'callid:(\\d+)',
      },
      dimensions: ['platform', 'version'],
      deviceField: 'deviceid',
      dedupeBy: 'callid',
      truncation: 'InCompleteResult|部分日志因超出行数限制|无法全部显示',
    },
    {
      id: 'builtin-source-ip',
      name: '来源 IP 统计',
      builtin: true,
      description: '提取请求日志的来源 IP，统计各 IP 请求量（单维度规则示例）',
      lineMatch: 'request url',
      fields: {
        ts: '\\d{4}-\\d{2}-\\d{2} \\d{2}:\\d{2}:\\d{2}',
        ip: ' ip:([^ ]+)',
        callid: 'callid:(\\d+)',
      },
      dimensions: ['ip'],
      deviceField: null,
      dedupeBy: 'callid',
      truncation: 'InCompleteResult|部分日志因超出行数限制|无法全部显示',
    },
  ];

  function compileRule(rule) {
    const fields = [];
    for (const [name, src] of Object.entries((rule && rule.fields) || {})) {
      try { fields.push({ name, re: new RegExp(src) }); } catch (e) { /* 跳过无效正则 */ }
    }
    return {
      lineMatch: (rule && rule.lineMatch) || null,
      trunc: rule && rule.truncation ? new RegExp(rule.truncation) : null,
      fields,
    };
  }

  function cleanText(text) {
    return String(text == null ? '' : text)
      .replace(/[\u001b\u009b]\[[0-9;]*m/g, '')             // ANSI 颜色码
      .replace(/[\u0000-\u0008\u000b-\u001f\u007f]/g, '');  // 控制字符
  }

  // 解析文本 -> { records, truncated, machineCount }
  function parse(text, rule) {
    const c = compileRule(rule || BUILTIN_RULES[0]);
    const records = [];
    let truncated = false;
    let machineCount = null;
    for (const raw of cleanText(text).split('\n')) {
      const line = raw.trim();
      if (!line) continue;
      if (c.trunc && c.trunc.test(line)) truncated = true;
      if (machineCount == null) {
        const mc = line.match(/相关机器数量[:：]\s*(\d+)/);
        if (mc) machineCount = parseInt(mc[1], 10);
      }
      if (c.lineMatch && line.indexOf(c.lineMatch) === -1) continue;
      const rec = {};
      let hit = false;
      for (const f of c.fields) {
        const m = line.match(f.re);
        if (m) { rec[f.name] = m[1] !== undefined ? m[1] : m[0]; hit = true; }
      }
      if (hit) records.push(rec);
    }
    return { records, truncated, machineCount };
  }

  // 记录去重键：优先去重字段，否则按规则字段顺序拼接
  function recordKey(rec, rule) {
    if (rule && rule.dedupeBy && rec[rule.dedupeBy]) return String(rec[rule.dedupeBy]);
    const order = rule && rule.fields ? Object.keys(rule.fields) : Object.keys(rec);
    return order.map(k => rec[k] || '').join('|');
  }

  // 通用统计：按 dimensions[0] 分组，组内再按 dimensions[1] 细分
  function computeStats(records, rule) {
    const dims = (rule && rule.dimensions) || [];
    const d1 = dims[0] || null;
    const d2 = dims[1] || null;
    const devF = (rule && rule.deviceField) || null;
    const groups = new Map();
    const allDev = new Set();
    let total = 0;
    for (const r of records) {
      total++;
      if (!d1) continue;
      const k1 = r[d1] || '(未知)';
      let g = groups.get(k1);
      if (!g) { g = { count: 0, sub: new Map(), dev: new Set(), subDev: new Map() }; groups.set(k1, g); }
      g.count++;
      if (d2) {
        const k2 = r[d2] || '(未知)';
        g.sub.set(k2, (g.sub.get(k2) || 0) + 1);
        if (devF && r[devF]) {
          let s = g.subDev.get(k2);
          if (!s) { s = new Set(); g.subDev.set(k2, s); }
          s.add(r[devF]);
        }
      }
      if (devF && r[devF]) { g.dev.add(r[devF]); allDev.add(r[devF]); }
    }
    return { total, groups, allDev: allDev.size, d1, d2, devF };
  }

  return { BUILTIN_RULES, parse, recordKey, computeStats, cleanText, compileRule };
})();
