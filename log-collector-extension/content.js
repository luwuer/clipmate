(() => {
  'use strict';

  // 幂等注入保护：manifest 自动注入与 chrome.scripting.executeScript 手动注入可能都会执行
  if (window.__logCollectorInjected) return;
  window.__logCollectorInjected = true;

  const TOKEN_RE = /__logcollect=([A-Za-z0-9]+)/;
  const cache = new Map();

  function sleep(ms) { return new Promise(r => setTimeout(r, ms)); }

  // 汇总页面全部可见文本（含同源 iframe），不依赖具体 DOM 结构
  function collectText() {
    let text = '';
    try { text += document.body ? document.body.innerText : ''; } catch (e) { /* ignore */ }
    try {
      document.querySelectorAll('iframe').forEach(f => {
        try {
          const d = f.contentDocument;
          if (d && d.body) text += '\n' + d.body.innerText;
        } catch (e) { /* 跨域忽略 */ }
      });
    } catch (e) { /* ignore */ }
    return text;
  }

  // 页面模式下，从 storage 读取收集器设置的当前解析规则
  async function storedRule() {
    try {
      const got = await chrome.storage.local.get('activeRule');
      if (got && got.activeRule) return got.activeRule;
    } catch (e) { /* ignore */ }
    return LogRules.BUILTIN_RULES[0];
  }

  /* ============ 响应规范化（JSON / HTML / 纯文本三种返回兼容） ============ */

  function normalizeResponse(text, contentType) {
    const raw = String(text == null ? '' : text);
    let json;
    if (/json/i.test(contentType) || /^\s*[[{]/.test(raw)) {
      try { json = JSON.parse(raw); } catch (e) { json = undefined; }
    }
    if (json !== undefined) {
      // 递归收集所有字符串值拼接（日志正文通常在某个字符串字段中，\n 已被 JSON.parse 还原）
      const parts = [];
      const walk = o => {
        if (o == null) return;
        if (typeof o === 'string') { if (o) parts.push(o); return; }
        if (typeof o === 'number' || typeof o === 'boolean') return;
        if (Array.isArray(o)) { o.forEach(walk); return; }
        Object.keys(o).forEach(k => walk(o[k]));
      };
      walk(json);
      return { text: parts.join('\n'), json };
    }
    if (/html/i.test(contentType)) {
      let t = raw
        .replace(/<script[\s\S]*?<\/script>/gi, '')
        .replace(/<style[\s\S]*?<\/style>/gi, '')
        .replace(/<br\s*\/?>/gi, '\n')
        .replace(/<\/(?:div|p|tr|li|pre|td)>/gi, '\n')
        .replace(/<[^>]+>/g, '');
      const ta = document.createElement('textarea');
      ta.innerHTML = t;
      return { text: ta.value, json: undefined };
    }
    return { text: raw, json: undefined };
  }

  function jsonTopKeys(j) {
    if (j == null) return null;
    if (Array.isArray(j)) return ['(array) len=' + j.length];
    if (typeof j === 'object') return Object.keys(j).slice(0, 20);
    return null;
  }

  // 寻找翻页游标字段（next_offset / cursor 等）
  function findNextOffset(j) {
    let next = null;
    const walk = (o, depth) => {
      if (next !== null || o == null || depth > 4) return;
      if (Array.isArray(o)) { o.forEach(x => walk(x, depth + 1)); return; }
      if (typeof o === 'object') {
        for (const k of Object.keys(o)) {
          if (/^(next_?offset|nextoffset|cursor|next_?id)$/i.test(k) && typeof o[k] === 'number') {
            next = o[k];
            return;
          }
          walk(o[k], depth + 1);
        }
      }
    };
    walk(j, 0);
    return next;
  }

  /* ============ API 直连模式：同源 fetch showlog/query ============ */

  async function apiQuery(url, rule) {
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), 120000);
    try {
      const resp = await fetch(url, {
        credentials: 'include',
        signal: ctl.signal,
        headers: { 'Accept': 'application/json, text/plain, */*' }
      });
      const text = await resp.text();
      clearTimeout(timer);
      const ct = resp.headers.get('content-type') || '';
      const norm = normalizeResponse(text, ct);
      const parsed = LogRules.parse(norm.text, rule || LogRules.BUILTIN_RULES[0]);
      return {
        ok: true,
        status: resp.status,
        contentType: ct,
        textLen: text.length,
        parsed,
        jsonTopKeys: norm.json !== undefined ? jsonTopKeys(norm.json) : null,
        nextOffset: norm.json !== undefined ? findNextOffset(norm.json) : null,
        head: text.slice(0, 200)
      };
    } catch (e) {
      clearTimeout(timer);
      return { ok: false, error: String(e && e.message || e) };
    }
  }

  /* ============ 页面抓取模式（兼容保底） ============ */

  // 等待页面文本稳定（结果为异步加载，需要轮询判定加载完成）
  async function waitStable(timeoutMs) {
    const start = Date.now();
    let last = '';
    let stable = 0;
    while (Date.now() - start < timeoutMs) {
      await sleep(700);
      const cur = collectText();
      if (cur === last) stable++;
      else { stable = 0; last = cur; }
      if (stable >= 3 && /执行完成|ret\s+\d/.test(cur)) return 'stable-done';
      if (stable >= 8) return 'stable';
    }
    return 'timeout';
  }

  // 推送结果给收集器页，失败自动重试（收集器可能尚未就绪）
  function push(result, left) {
    if (left <= 0) return;
    try {
      chrome.runtime.sendMessage(result, () => {
        if (chrome.runtime.lastError) setTimeout(() => push(result, left - 1), 1500);
      });
    } catch (e) {
      setTimeout(() => push(result, left - 1), 1500);
    }
  }

  /* ============ 消息入口 ============ */
  chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
    // 存活探测：同步应答，供收集器确认内容脚本可用
    if (msg && msg.type === 'LOGCOLLECT_ECHO') {
      sendResponse({ pong: true, href: location.href });
      return false;
    }
    // API 直连：由收集器下发接口 URL 与解析规则，此处同源请求并解析
    if (msg && msg.type === 'LOGCOLLECT_API_QUERY' && msg.url) {
      apiQuery(msg.url, msg.rule).then(sendResponse);
      return true; // 异步回复
    }
    // 页面模式兜底轮询
    if (msg && msg.type === 'LOGCOLLECT_PING') {
      const r = cache.get(msg.token);
      if (r) sendResponse(r);
    }
    return false;
  });

  // 页面模式：URL 带 token 时自动采集
  const m = location.href.match(TOKEN_RE);
  if (m) {
    (async () => {
      const reason = await waitStable(90000);
      const text = collectText();
      const rule = await storedRule();
      const parsed = LogRules.parse(text, rule);
      const result = Object.assign(parsed, {
        type: 'LOGCOLLECT_RESULT',
        token: m[1],
        reason,
        href: location.href,
        textLen: text.length
      });
      if (cache.size > 20) cache.clear();
      cache.set(m[1], result);
      push(result, 25);
    })();
  }
})();
