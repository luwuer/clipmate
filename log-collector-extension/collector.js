'use strict';

/* ============================================================
 * 日志自循环收集分析器 - 收集器页
 * UI：Vue 3（runtime 构建 + h() 渲染函数）
 *     注意：MV3 扩展页 CSP 禁止 eval/new Function，
 *     不能使用带模板编译器的完整版 Vue，故全部用渲染函数编写。
 * 解析：LogRules 多规则引擎（见 rules.js）
 * ============================================================ */
const { createApp, h, reactive, ref, watch, nextTick, onMounted } = Vue;

/* ================= 常量 ================= */
const PLATFORM_COLORS = ['#4f7cff', '#00b96b', '#ff8800', '#eb2f96', '#722ed1', '#13c2c2', '#faad14'];
const HOST_HOME = 'https://log.wwitil.woa.com/logsearch.html';
// 惰性承载页：服务端 404 也无妨——没有应用脚本、不会自行跳转/刷新，仅用于同源 fetch
const HOST_INERT = 'https://log.wwitil.woa.com/__log_collector_host__';
const PORT_ERR_RE = /Receiving end does not exist|message port closed|Could not establish/i;
const LOG_CAP = 800;

/* ================= 非响应式重数据（避免 Vue 深度代理大集合） ================= */
const seen = new Map();   // key -> record
const runState = { tabId: null, apiTabId: null, recoveryTabs: 0, stopRequested: false, scriptingWarned: false };

/* ================= 响应式状态 ================= */
const state = reactive({
  // 查询配置
  tplUrl: '', apiTpl: '', mode: 'api', quickRange: '12',
  beginTime: '', endTime: '',
  winSize: 60, lineCnt: 0, offsetStep: 2000, minWin: 10, maxQ: 200, fullMode: false,
  // 解析规则
  rules: [], activeRuleId: '',
  editorOpen: false, draft: null,
  // 运行状态
  running: false, statusText: '就绪',
  logs: [], queryCount: 0, truncatedCount: 0, failedCount: 0,
  recordCount: 0, stats: null,
});

/* ================= 工具函数 ================= */
const sleep = ms => new Promise(r => setTimeout(r, ms));
const pad = n => String(n).padStart(2, '0');
const esc = s => String(s).replace(/[&<>"']/g, c =>
  ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const pct = (n, t) => (t ? n / t * 100 : 0);

function fmt(d) {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
         `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}
function dtToInput(d) {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
function inputToDT(v) { return new Date(v && v.length === 16 ? v + ':00' : v); }
function stamp() {
  const d = new Date();
  return `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}-${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`;
}

let logSeq = 0;
function log(msg, cls) {
  state.logs.push({ key: ++logSeq, msg, cls });
  while (state.logs.length > LOG_CAP) state.logs.shift();
}

/* ================= 规则管理 ================= */
function activeRuleRaw() {
  return state.rules.find(r => r.id === state.activeRuleId) || state.rules[0] || null;
}

function setRule(id) {
  if (!state.rules.some(r => r.id === id)) return;
  state.activeRuleId = id;
  persistRules();
  refreshStats();
}

function persistRules() {
  const customRules = state.rules.filter(r => !r.builtin);
  chrome.storage.local.set({
    customRules,
    activeRuleId: state.activeRuleId,
    activeRule: activeRuleRaw(),
  });
}

function draftFromRule(rule) {
  const dims = rule.dimensions || [];
  return {
    id: rule.builtin ? '' : rule.id,
    name: rule.builtin ? rule.name + ' 副本' : rule.name,
    lineMatch: rule.lineMatch || '',
    truncation: rule.truncation || '',
    fields: Object.entries(rule.fields || {}).map(([name, re]) => ({ name, re })),
    dims: [dims[0] || '', dims[1] || ''],
    deviceField: rule.deviceField || '',
    dedupeBy: rule.dedupeBy || '',
  };
}

function ruleFromDraft(d) {
  const fields = {};
  for (const f of d.fields) if (f.name && f.re) fields[f.name] = f.re;
  return {
    id: d.id || ('rule-' + Date.now().toString(36)),
    name: d.name.trim(),
    lineMatch: d.lineMatch.trim(),
    truncation: d.truncation.trim(),
    fields,
    dimensions: [d.dims[0], d.dims[1]].filter(Boolean),
    deviceField: d.deviceField || null,
    dedupeBy: d.dedupeBy || null,
  };
}

function validateDraft(d) {
  if (!d.name.trim()) return '请填写规则名称';
  const fields = d.fields.filter(f => f.name.trim() || f.re.trim());
  if (!fields.length) return '至少配置一个字段提取规则';
  const names = fields.map(f => f.name.trim());
  if (names.some(n => !n)) return '存在未命名的字段';
  if (new Set(names).size !== names.length) return '字段名重复';
  for (const f of fields) {
    try { new RegExp(f.re); } catch (e) { return `字段「${f.name}」的正则无效：${e.message}`; }
  }
  if (d.truncation.trim()) {
    try { new RegExp(d.truncation); } catch (e) { return `截断标记正则无效：${e.message}`; }
  }
  const nameSet = new Set(names);
  if (!d.dims[0] || !nameSet.has(d.dims[0])) return '请选择有效的主统计维度';
  if (d.dims[1] && !nameSet.has(d.dims[1])) return '次统计维度无效';
  return null;
}

function openEditor(rule) {
  state.draft = rule ? draftFromRule(rule) : {
    id: '', name: '', lineMatch: '', truncation: '',
    fields: [{ name: '', re: '' }], dims: ['', ''], deviceField: '', dedupeBy: '',
  };
  state.editorOpen = true;
}

function saveDraft() {
  const d = state.draft;
  const err = validateDraft(d);
  if (err) { alert(err); return; }
  const rule = ruleFromDraft(d);
  const idx = state.rules.findIndex(r => r.id === rule.id);
  if (idx >= 0) state.rules.splice(idx, 1, rule);
  else state.rules.push(rule);
  state.activeRuleId = rule.id;
  state.editorOpen = false;
  persistRules();
  refreshStats();
  log(`解析规则已保存并启用：${rule.name}`);
}

function deleteRule(id) {
  const rule = state.rules.find(r => r.id === id);
  if (!rule || rule.builtin) return;
  if (!confirm(`确认删除规则「${rule.name}」？`)) return;
  state.rules = state.rules.filter(r => r.id !== id);
  if (state.activeRuleId === id) state.activeRuleId = state.rules[0] ? state.rules[0].id : '';
  persistRules();
  refreshStats();
  log(`已删除规则：${rule.name}`);
}

/* ================= URL 构造 ================= */
// 保留模板中的查询参数，替换 overrides 指定的项；token 用于页面模式标记
function rebuildUrl(tpl, overrides, dropKeys, token) {
  const hashIdx = tpl.indexOf('#');
  const base = hashIdx >= 0 ? tpl.slice(0, hashIdx) : tpl;
  const qIdx = base.indexOf('?');
  const origin = qIdx >= 0 ? base.slice(0, qIdx) : base;
  const qs = qIdx >= 0 ? base.slice(qIdx + 1) : '';
  const params = [];
  for (const pair of qs.split('&')) {
    if (!pair) continue;
    const eq = pair.indexOf('=');
    const k = eq >= 0 ? decodeURIComponent(pair.slice(0, eq)) : pair;
    const v = eq >= 0 ? decodeURIComponent(pair.slice(eq + 1)) : '';
    if (dropKeys.has(k) || k === '__logcollect' ||
        Object.prototype.hasOwnProperty.call(overrides, k)) continue;
    params.push([k, v]);
  }
  for (const k of Object.keys(overrides)) params.push([k, overrides[k]]);
  if (token) params.push(['__logcollect', token]);
  const query = params.map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`).join('&');
  return token ? `${origin}?${query}#__logcollect=${token}` : `${origin}?${query}`;
}

function buildApiUrl(apiTpl, b, e, offset, lineCnt) {
  return rebuildUrl(apiTpl, {
    begintime: fmt(b),
    endtime: fmt(e),
    offset: String(offset),
    line_cnt: String(lineCnt),
    _: String(Date.now())
  }, new Set(), null);
}

// logsearch.html 页面 URL -> showlog/query 接口 URL
function toApiTemplate(tpl) {
  let u;
  try { u = new URL(String(tpl || '').trim()); } catch (e) { return null; }
  if (!/(^|\.)log\.wwitil\.woa\.com$/.test(u.hostname)) return null;
  if (u.pathname.indexOf('/showlog/query') >= 0) return u.toString();
  if (/logsearch\.html?$/.test(u.pathname)) {
    u.pathname = u.pathname.replace(/logsearch\.html?$/, 'showlog/query');
    return u.toString();
  }
  return null;
}

/* ================= 宿主标签页管理 ================= */
function scriptingAvailable() {
  return !!(chrome && chrome.scripting && typeof chrome.scripting.executeScript === 'function');
}

// 存活探测：内容脚本同步应答，确认监听器可用
function echoTab(tabId) {
  return new Promise(resolve => {
    const timer = setTimeout(() => resolve(null), 3000);
    try {
      chrome.tabs.sendMessage(tabId, { type: 'LOGCOLLECT_ECHO' }, resp => {
        clearTimeout(timer);
        if (chrome.runtime.lastError) resolve(null);
        else resolve(resp || null);
      });
    } catch (e) { clearTimeout(timer); resolve(null); }
  });
}

// 等待页面加载完成且 URL 稳定（规避页面加载后异步跳转/自刷新的时序问题）
async function waitForSettled(tabId, maxMs = 20000) {
  const start = Date.now();
  let lastUrl = null;
  let stableSince = 0;
  while (Date.now() - start < maxMs) {
    let status = '', url = '';
    try {
      const t = await chrome.tabs.get(tabId);
      status = t.status || '';
      url = t.url || '';
    } catch (e) { return; }
    const now = Date.now();
    if (url === lastUrl) {
      if (stableSince && now - stableSince >= 1500 && status === 'complete') return;
    } else {
      lastUrl = url;
      stableSince = now;
    }
    await sleep(400);
  }
}

// 主动注入内容脚本（幂等；浏览器不支持 chrome.scripting 时返回 false，靠新页面自动注入兜底）
async function injectContentScript(tabId) {
  if (!scriptingAvailable()) return false;
  try {
    await chrome.scripting.executeScript({ target: { tabId }, files: ['content.js'] });
    return true;
  } catch (e) {
    log(`内容脚本注入失败：${e && e.message ? e.message : e}`, 'err');
    return false;
  }
}

// 创建并准备好承载用的宿主标签页：等待加载稳定 + 内容脚本可应答
async function createHostTab(url) {
  const tab = await chrome.tabs.create({ url, active: false });
  runState.apiTabId = tab.id;
  await waitForSettled(tab.id);
  for (let i = 0; i < 6; i++) {
    if (await echoTab(tab.id)) return tab.id;
    await injectContentScript(tab.id);
    await sleep(1000);
  }
  return tab.id;
}

async function ensureApiTab() {
  if (runState.apiTabId != null) {
    if (await echoTab(runState.apiTabId)) return runState.apiTabId;
    runState.apiTabId = null;
  }

  if (!scriptingAvailable() && !runState.scriptingWarned) {
    runState.scriptingWarned = true;
    log('当前浏览器未提供 chrome.scripting（若为 Chrome，可在 chrome://extensions 先移除再重新加载插件以启用该能力），插件将改用独立标签页承载请求', 'warn');
  }

  // 优先复用已打开的页面：探测内容脚本是否存活（扩展重载后旧页面无脚本，探测失败则跳过）
  const tabs = await chrome.tabs.query({ url: '*://log.wwitil.woa.com/*' });
  const sorted = tabs.slice(0, 4).sort((a, b) =>
    (/page=query/.test(a.url || '') ? 1 : 0) - (/page=query/.test(b.url || '') ? 1 : 0));
  for (const t of sorted) {
    let echo = await echoTab(t.id);
    if (!echo && await injectContentScript(t.id)) {
      await sleep(300);
      echo = await echoTab(t.id);
    }
    if (echo) {
      runState.apiTabId = t.id;
      log(`复用标签页 #${t.id} 承载接口请求：${(t.url || '').slice(0, 90)}`);
      return runState.apiTabId;
    }
  }

  log('未找到可复用的宿主页，新开一个后台标签页承载请求（请确认浏览器已登录 log.wwitil.woa.com）');
  let tabId = await createHostTab(HOST_HOME);
  if (!(await echoTab(tabId))) {
    log('宿主页未就绪，改用惰性承载页重试…', 'warn');
    tabId = await createHostTab(HOST_INERT);
  }
  return tabId;
}

function apiQueryViaTab(tabId, url) {
  return new Promise(resolve => {
    const timer = setTimeout(() => resolve({ ok: false, error: 'timeout' }), 130000);
    try {
      chrome.tabs.sendMessage(tabId, {
        type: 'LOGCOLLECT_API_QUERY',
        url,
        rule: activeRuleRaw(),
      }, resp => {
        clearTimeout(timer);
        if (chrome.runtime.lastError) resolve({ ok: false, error: chrome.runtime.lastError.message });
        else resolve(resp || { ok: false, error: 'empty response' });
      });
    } catch (e) {
      clearTimeout(timer);
      resolve({ ok: false, error: String(e) });
    }
  });
}

// 发送 API 请求；端口类错误（页面自刷新/跳转/无内容脚本）多级恢复
async function apiQuery(url) {
  let tabId = await ensureApiTab();
  let res = await apiQueryViaTab(tabId, url);
  if (res.ok || !PORT_ERR_RE.test(res.error || '')) return res;

  // 恢复1：宿主页可能正在刷新/跳转中，等其稳定后重试（刷新后的新页面会自动注入内容脚本）
  for (let i = 0; i < 2 && !res.ok; i++) {
    log(`请求未送达，等待宿主页稳定后重试（${i + 1}/2）…`, 'warn');
    await sleep(1500);
    if (!(await echoTab(tabId))) {
      await injectContentScript(tabId);
      await sleep(300);
    }
    if (await echoTab(tabId)) {
      res = await apiQueryViaTab(tabId, url);
    }
  }
  if (res.ok) return res;

  // 恢复2：换一个新开的惰性宿主标签页（无应用脚本，不会自行跳转）
  if (runState.recoveryTabs < 3) {
    runState.recoveryTabs++;
    log('仍无响应，新开一个惰性宿主标签页重试…', 'warn');
    try {
      tabId = await createHostTab(HOST_INERT);
      res = await apiQueryViaTab(tabId, url);
    } catch (e) {
      log('新开宿主标签页失败：' + (e && e.message ? e.message : e), 'err');
    }
  }
  return res;
}

/* ================= 采集循环 ================= */
function mergeRecords(records) {
  const rule = activeRuleRaw();
  let added = 0;
  for (const r of records) {
    const key = LogRules.recordKey(r, rule);
    if (!seen.has(key)) { seen.set(key, r); added++; }
  }
  return added;
}

function splitWindows(b, e, winMs) {
  const list = [];
  let cur = new Date(b.getTime());
  while (cur < e) {
    const end = new Date(Math.min(cur.getTime() + winMs, e.getTime()));
    list.push({ b: cur, e: end });
    cur = end;
  }
  return list;
}

function logApiResponse(prefix, res, p, added) {
  const kb = (res.textLen / 1024).toFixed(0);
  log(`${prefix} -> HTTP ${res.status} ${res.contentType || '?'} ${kb}KB，` +
      `${p.records.length} 条（新增 ${added}）${p.truncated ? '，截断' : '，完整'}` +
      (res.jsonTopKeys ? `，json keys=[${res.jsonTopKeys.join(',')}]` : ''));
  if (!p.records.length && res.textLen > 0) {
    log(`  响应预览：${(res.head || '').replace(/\s+/g, ' ').slice(0, 180)}`, 'warn');
  }
}

async function runApiLoop(cfg) {
  runState.recoveryTabs = 0;
  const rule = activeRuleRaw();
  const queue = splitWindows(cfg.b, cfg.e, cfg.winMs);
  const totalWindows = queue.length;
  let winIdx = 0;
  let consecutiveFail = 0;

  log(`== 开始收集（接口直连）：${fmt(cfg.b)} ~ ${fmt(cfg.e)}，共 ${totalWindows} 个窗口（每窗口 ${cfg.winMs / 60000} 分钟）；` +
      `解析规则：${rule.name}；` +
      (cfg.fullMode
        ? `分页拉全：截断时按 offset 步进 ${cfg.step} 翻页（同 continueQuery）`
        : `快速模式：每窗口仅取第一页（line_cnt=${cfg.lineCnt === 0 ? '默认约2000' : cfg.lineCnt}），可能不足`));

  while (queue.length && !runState.stopRequested && state.queryCount < cfg.maxQ) {
    const w = queue.shift();
    winIdx++;

    if (!cfg.fullMode) {
      /* ---- 快速模式：每窗口单次请求 ---- */
      state.statusText = `窗口 ${winIdx}：${fmt(w.b)} ~ ${fmt(w.e)}`;
      const url = buildApiUrl(cfg.apiTpl, w.b, w.e, 0, cfg.lineCnt);
      const res = await apiQuery(url);
      state.queryCount++;

      if (!res.ok) {
        state.failedCount++;
        consecutiveFail++;
        log(`[失败] 窗口 ${winIdx} ${fmt(w.b)} ~ ${fmt(w.e)}：${res.error}`, 'err');
        if (consecutiveFail >= 3) {
          log('连续 3 次请求失败，已停止。请确认浏览器已登录 log.wwitil.woa.com（登录态失效、宿主标签页被关闭或跳转都会导致此问题），修正后重新开始。', 'err');
          break;
        }
      } else {
        consecutiveFail = 0;
        const p = res.parsed || { records: [], truncated: false };
        const added = mergeRecords(p.records || []);
        if (p.truncated) state.truncatedCount++;
        logApiResponse(`[窗口 ${winIdx}/${totalWindows}] ${fmt(w.b)}~${fmt(w.e)}`, res, p, added);
        if (p.truncated) log('  该窗口超出单页上限，快速模式仅取第一页；如需拉全请勾选「窗口内分页拉全」', 'warn');
      }
    } else {
      /* ---- 完整模式：分页拉全（offset 递增翻页） ---- */
      let offset = 0;
      let pages = 0;
      while (pages < 100 && !runState.stopRequested && state.queryCount < cfg.maxQ) {
        state.statusText = `窗口 ${winIdx}：${fmt(w.b)} ~ ${fmt(w.e)} 第 ${pages + 1} 页 offset=${offset}`;
        const url = buildApiUrl(cfg.apiTpl, w.b, w.e, offset, cfg.lineCnt);
        const res = await apiQuery(url);
        state.queryCount++;

        if (!res.ok) {
          state.failedCount++;
          consecutiveFail++;
          log(`[失败] 窗口 ${winIdx} ${fmt(w.b)} ~ ${fmt(w.e)} offset=${offset}：${res.error}`, 'err');
          if (consecutiveFail >= 3) {
            log('连续 3 次请求失败，已停止。请确认浏览器已登录 log.wwitil.woa.com（登录态失效、宿主标签页被关闭或跳转都会导致此问题），修正后重新开始。', 'err');
            runState.stopRequested = true;
          }
          break;
        }
        consecutiveFail = 0;

        const p = res.parsed || { records: [], truncated: false };
        const added = mergeRecords(p.records || []);
        pages++;
        logApiResponse(`[窗口 ${winIdx}/${totalWindows}] ${fmt(w.b)}~${fmt(w.e)} 第 ${pages} 页 offset=${offset}`, res, p, added);

        if (!p.records.length) break;          // 空页：本窗口结束
        if (!p.truncated) break;               // 完整：本窗口结束

        state.truncatedCount++;
        // 截断：优先用响应自带游标翻页
        const next = res.nextOffset;
        if (next != null && next !== offset) {
          offset = next;
          continue;
        }
        // 翻页无新数据：offset 语义可能不符，二分时间窗兜底
        if (added === 0) {
          if (w.e - w.b > cfg.minWinMs) {
            const mid = new Date(Math.floor((w.b.getTime() + w.e.getTime()) / 2000) * 1000);
            queue.unshift({ b: w.b, e: mid }, { b: mid, e: w.e });
            log('  翻页未带来新数据，二分时间窗兜底', 'warn');
          } else {
            log('  已达最小二分窗口仍截断，接受部分数据', 'warn');
          }
          break;
        }
        offset += cfg.step;
      }
    }

    refreshStats();
    saveSession();
    await sleep(300);
  }

  const stopped = runState.stopRequested;
  state.statusText = '已完成';
  log(stopped
    ? `[停止] 手动停止或异常终止，剩余 ${queue.length} 个窗口未查询`
    : `[结束] 共处理 ${winIdx} 个窗口、${state.queryCount} 次请求，累计 ${seen.size} 条记录（截断页 ${state.truncatedCount} / 失败 ${state.failedCount}）`);
  saveSession();
}

/* ================= 页面模式（兼容保底） ================= */
function queryOnce(url, token) {
  return new Promise(resolve => {
    let done = false;
    let timeout = null;
    let pingTimer = null;
    let listener = null;

    const cleanup = () => {
      if (timeout) clearTimeout(timeout);
      if (pingTimer) clearInterval(pingTimer);
      if (listener) chrome.runtime.onMessage.removeListener(listener);
    };
    const finish = res => { if (done) return; done = true; cleanup(); resolve(res); };

    listener = msg => {
      if (msg && msg.type === 'LOGCOLLECT_RESULT' && msg.token === token) finish(msg);
    };
    chrome.runtime.onMessage.addListener(listener);

    timeout = setTimeout(() => finish(null), 150000);

    pingTimer = setInterval(() => {
      if (runState.tabId == null) return;
      try {
        chrome.tabs.sendMessage(runState.tabId, { type: 'LOGCOLLECT_PING', token }, resp => {
          void chrome.runtime.lastError;
          if (resp && resp.token === token) finish(resp);
        });
      } catch (e) { /* ignore */ }
    }, 4000);

    (async () => {
      try {
        if (runState.tabId == null) {
          const tab = await chrome.tabs.create({ url, active: true });
          runState.tabId = tab.id;
        } else {
          await chrome.tabs.update(runState.tabId, { url });
        }
      } catch (e) {
        try {
          const tab = await chrome.tabs.create({ url, active: true });
          runState.tabId = tab.id;
        } catch (e2) { finish(null); }
      }
    })();
  });
}

async function runPageLoop(cfg) {
  const rule = activeRuleRaw();
  const queue = splitWindows(cfg.b, cfg.e, cfg.winMs);
  const totalWindows = queue.length;
  let winIdx = 0;
  let consecutiveFail = 0;
  log(`== 开始收集（页面抓取）：${fmt(cfg.b)} ~ ${fmt(cfg.e)}，共 ${totalWindows} 个窗口；解析规则：${rule.name}（最小二分 ${cfg.minWinMs / 60000} 分钟，最多 ${cfg.maxQ} 次查询）`);

  while (queue.length && !runState.stopRequested && state.queryCount < cfg.maxQ) {
    const w = queue.shift();
    winIdx++;
    const token = 't' + Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
    const url = rebuildUrl(state.tplUrl.trim(), {
      begintime: fmt(w.b), endtime: fmt(w.e)
    }, new Set(['offset']), token);

    state.statusText = `窗口 ${winIdx}：${fmt(w.b)} ~ ${fmt(w.e)}`;
    const res = await queryOnce(url, token);
    state.queryCount++;

    if (!res || (!res.records || res.records.length === 0)) {
      state.failedCount++;
      consecutiveFail++;
      log(`[失败] 窗口 ${winIdx} ${fmt(w.b)} ~ ${fmt(w.e)} 未获取到记录` +
          (res && res.reason === 'timeout' ? '（等待超时）' : ''), 'err');
      if (consecutiveFail >= 3) {
        log('连续 3 次未获取到记录，已停止。页面模式受前端渲染行数限制，建议改用「接口直连」模式。', 'err');
        break;
      }
    } else {
      consecutiveFail = 0;
      const added = mergeRecords(res.records || []);
      if (res.truncated) {
        state.truncatedCount++;
        if (w.e - w.b > cfg.minWinMs) {
          const mid = new Date(Math.floor((w.b.getTime() + w.e.getTime()) / 2000) * 1000);
          queue.unshift({ b: w.b, e: mid }, { b: mid, e: w.e });
          log(`[截断] 窗口 ${winIdx}/${totalWindows} ${fmt(w.b)} ~ ${fmt(w.e)} -> ${res.records.length} 条（新增 ${added}），拆分两半继续`);
        } else {
          log(`[警告] 窗口 ${winIdx}/${totalWindows} ${fmt(w.b)} ~ ${fmt(w.e)} -> ${res.records.length} 条（新增 ${added}），已达最小窗口仍截断，接受部分数据`, 'warn');
        }
      } else {
        log(`[完整] 窗口 ${winIdx}/${totalWindows} ${fmt(w.b)} ~ ${fmt(w.e)} -> ${res.records.length} 条（新增 ${added}）`);
      }
      refreshStats();
      saveSession();
    }
    await sleep(500);
  }

  const stopped = runState.stopRequested;
  state.statusText = '已完成';
  log(stopped
    ? `[停止] 手动停止，剩余 ${queue.length} 个窗口未查询`
    : `[结束] 共 ${state.queryCount} 次查询，累计 ${seen.size} 条记录`);
  saveSession();
}

/* ================= 入口 ================= */
async function start() {
  if (state.running) return;
  const tpl = state.tplUrl.trim();
  if (!tpl || !/^https?:\/\//.test(tpl)) { alert('请先填写日志查询页 URL'); return; }
  if (!tpl.includes('log.wwitil.woa.com')) { alert('URL 不是 log.wwitil.woa.com，请检查'); return; }
  if (!state.beginTime || !state.endTime) { alert('请填写开始/结束时间（或选择快捷时间范围）'); return; }
  const rule = activeRuleRaw();
  if (!rule) { alert('请先配置解析规则'); return; }

  let b, e;
  try { b = inputToDT(state.beginTime); e = inputToDT(state.endTime); }
  catch (err) { alert('开始/结束时间格式不正确'); return; }
  if (!(e > b)) { alert('结束时间需晚于开始时间'); return; }

  const lineCnt = Math.max(0, Math.floor(state.lineCnt) || 0);
  const stepInput = Math.floor(state.offsetStep);
  const cfg = {
    b, e,
    winMs: Math.max(1, state.winSize || 60) * 60000,
    minWinMs: Math.max(1, state.minWin || 10) * 60000,
    maxQ: Math.max(1, state.maxQ || 200),
    lineCnt,
    step: stepInput > 0 ? stepInput : (lineCnt > 0 ? lineCnt : 2000),
    fullMode: state.fullMode,
  };

  if (state.mode === 'api') {
    const apiTpl = state.apiTpl.trim();
    if (!apiTpl || !/^https?:\/\/log\.wwitil\.woa\.com\/showlog\/query/.test(apiTpl)) {
      alert('查询接口 URL 无效。请粘贴 DevTools Network 中 showlog/query 的请求 URL，或确认上方页面 URL 可自动转换。');
      return;
    }
    cfg.apiTpl = apiTpl;
  }

  state.running = true;
  runState.stopRequested = false;
  chrome.storage.local.set({
    tplUrl: tpl,
    apiTpl: state.apiTpl.trim(),
    mode: state.mode,
    lineCnt,
    winSize: cfg.winMs / 60000,
    fullMode: cfg.fullMode,
    offsetStep: stepInput > 0 ? stepInput : '',
    activeRule: rule,
  });

  try {
    if (state.mode === 'api') await runApiLoop(cfg);
    else await runPageLoop(cfg);
  } catch (err) {
    log('发生错误：' + (err && err.message ? err.message : err), 'err');
  }
  state.running = false;
  runState.stopRequested = false;
}

function stop() {
  runState.stopRequested = true;
  state.statusText = '停止中…';
}

/* ================= 统计与导出 ================= */
function refreshStats() {
  const rule = activeRuleRaw();
  const st = LogRules.computeStats([...seen.values()], rule);
  state.stats = st.total ? st : null;
  state.recordCount = seen.size;
}

function download(filename, content, mime) {
  const blob = new Blob([content], { type: mime });
  const a = document.createElement('a');
  a.href = URL.createObjectURL(blob);
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(a.href), 5000);
}

const REPORT_CSS = `
  * { box-sizing: border-box; }
  body { margin:0; font-family:-apple-system,"PingFang SC","Segoe UI","Microsoft YaHei",sans-serif;
         background:#f5f7fb; color:#1f2329; }
  .container { max-width:1080px; margin:0 auto; padding:32px 24px 60px; }
  header { background:linear-gradient(135deg,#4f7cff 0%,#722ed1 100%); color:#fff;
           border-radius:16px; padding:28px 32px; margin-bottom:24px; }
  header h1 { margin:0 0 8px; font-size:24px; }
  header .meta { font-size:13px; opacity:.9; line-height:1.8; }
  .cards { display:grid; grid-template-columns:repeat(auto-fit,minmax(180px,1fr)); gap:16px; margin-bottom:28px; }
  .card { background:#fff; border-radius:14px; padding:20px; box-shadow:0 1px 4px rgba(31,35,41,.06); text-align:center; }
  .card-num { font-size:28px; font-weight:700; color:#4f7cff; }
  .card-label { margin-top:6px; font-size:13px; color:#646a73; }
  h3 { font-size:18px; margin:28px 0 12px; }
  h3 .sub { font-size:13px; font-weight:400; color:#8a919f; margin-left:10px; }
  table { width:100%; border-collapse:collapse; background:#fff; border-radius:12px; overflow:hidden;
          box-shadow:0 1px 4px rgba(31,35,41,.06); font-size:14px; margin-bottom:10px; }
  th, td { padding:10px 14px; text-align:left; border-bottom:1px solid #f0f2f5; }
  th { background:#fafbfc; color:#646a73; font-weight:600; font-size:13px; }
  tbody tr:hover { background:#f7f9ff; }
  tbody tr:last-child td { border-bottom:none; }
  td.num { font-variant-numeric:tabular-nums; white-space:nowrap; }
  .dot { display:inline-block; width:10px; height:10px; border-radius:3px; margin-right:8px; vertical-align:1px; }
  code { background:#f2f4f7; padding:2px 8px; border-radius:6px; font-size:13px; }
  .bar-wrap { background:#eef1f6; border-radius:99px; height:10px; width:100%; min-width:120px; overflow:hidden; }
  .bar { height:100%; border-radius:99px; }
  footer { margin-top:40px; font-size:12px; color:#8a919f; text-align:center; }
`;

function reportSourceLine() {
  try {
    const u = new URL(state.apiTpl.trim() || state.tplUrl.trim());
    let src = `${u.host}${u.pathname}`;
    const kw = u.searchParams.get('keywords');
    if (kw) src += ` · keywords=${kw.replace(/[\r\n]+/g, ' ')}`;
    return src;
  } catch (e) { return state.tplUrl.slice(0, 120); }
}

function buildReportHtml() {
  const rule = activeRuleRaw();
  const st = LogRules.computeStats([...seen.values()], rule);
  const now = fmt(new Date());
  const groups = [...st.groups.entries()].sort((a, b) => b[1].count - a[1].count);
  const colors = {};
  groups.forEach(([k], i) => colors[k] = PLATFORM_COLORS[i % PLATFORM_COLORS.length]);
  const d1 = st.d1 || '维度';
  const d2 = st.d2;
  const hasDev = !!st.devF;

  const cards = `<div class="cards">` +
    `<div class="card"><div class="card-num">${st.total.toLocaleString()}</div><div class="card-label">有效记录</div></div>` +
    `<div class="card"><div class="card-num">${groups.length}</div><div class="card-label">${esc(d1)} 取值数</div></div>` +
    (hasDev ? `<div class="card"><div class="card-num">${st.allDev.toLocaleString()}</div><div class="card-label">独立设备</div></div>` : '') +
    `<div class="card"><div class="card-num">${state.queryCount}</div><div class="card-label">请求次数</div></div>` +
    `</div>`;

  const barHtml = (ratio, color) =>
    `<div class="bar-wrap"><div class="bar" style="width:${Math.min(ratio * 100, 100).toFixed(2)}%;background:${color};"></div></div>`;

  let html = cards;
  html += `<h3>${esc(d1)} 占比 <span class="sub">共 ${st.total.toLocaleString()} 条</span></h3>
    <table><thead><tr><th>${esc(d1)}</th><th>记录数</th><th>占比</th><th style="width:30%">分布</th>` +
    (hasDev ? '<th>独立设备</th>' : '') + (d2 ? `<th>${esc(d2)} 数</th>` : '') + `</tr></thead><tbody>`;
  for (const [k, g] of groups) {
    const r = pct(g.count, st.total);
    html += `<tr><td><span class="dot" style="background:${colors[k]}"></span><b>${esc(k)}</b></td>
      <td class="num">${g.count.toLocaleString()}</td><td class="num"><b>${r.toFixed(2)}%</b></td>
      <td>${barHtml(r / 100, colors[k])}</td>` +
      (hasDev ? `<td class="num">${g.dev.size.toLocaleString()}</td>` : '') +
      (d2 ? `<td class="num">${g.sub.size}</td>` : '') + `</tr>`;
  }
  html += '</tbody></table>';

  if (d2) {
    for (const [k, g] of groups) {
      html += `<h3><span class="dot" style="background:${colors[k]}"></span>${esc(k)}
        <span class="sub">${g.count.toLocaleString()} 条 · ${g.sub.size} 个取值 · 占全部 ${pct(g.count, st.total).toFixed(2)}%</span></h3>
        <table><thead><tr><th>${esc(d2)}</th><th>记录数</th><th>组内占比</th><th style="width:36%">分布</th>` +
        (hasDev ? '<th>独立设备</th>' : '') + `</tr></thead><tbody>`;
      for (const [v, c] of [...g.sub.entries()].sort((a, b) => b[1] - a[1])) {
        const r = pct(c, g.count);
        html += `<tr><td><code>${esc(v)}</code></td><td class="num">${c.toLocaleString()}</td>
          <td class="num"><b>${r.toFixed(2)}%</b></td><td>${barHtml(r / 100, colors[k])}</td>` +
          (hasDev ? `<td class="num">${((g.subDev.get(v) || new Set()).size).toLocaleString()}</td>` : '') + `</tr>`;
      }
      html += '</tbody></table>';
    }
  }

  return `<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${esc(rule.name)} 报告（日志自循环采集）</title>
<style>${REPORT_CSS}</style>
</head>
<body>
<div class="container">
  <header>
    <h1>${esc(rule.name)} 报告</h1>
    <div class="meta">
      数据来源：${esc(reportSourceLine())}<br>
      生成时间：${now} · 采集 ${state.queryCount} 次请求 · ${st.total.toLocaleString()} 条记录` +
      (rule.dedupeBy ? `（按 ${esc(rule.dedupeBy)} 去重）` : '') + `
    </div>
  </header>
  ${html}
  <footer>由「日志自循环收集分析器」Chrome 扩展自动生成</footer>
</div>
</body>
</html>`;
}

function exportHtml() {
  if (!seen.size) { alert('暂无数据，请先采集'); return; }
  download(`log-report-${stamp()}.html`, buildReportHtml(), 'text/html;charset=utf-8');
  log('已导出 HTML 报告');
}

function exportCsv() {
  if (!seen.size) { alert('暂无数据，请先采集'); return; }
  const rule = activeRuleRaw();
  const cols = Object.keys(rule.fields || {});
  if (!cols.length) { alert('当前规则无字段定义'); return; }
  const rows = [cols];
  const tsCol = cols.includes('ts') ? 'ts' : null;
  const list = [...seen.values()].sort((a, b) => {
    if (tsCol) return (a[tsCol] || '') < (b[tsCol] || '') ? -1 : (a[tsCol] || '') > (b[tsCol] || '') ? 1 : 0;
    return 0;
  });
  for (const r of list) rows.push(cols.map(c => r[c] || ''));
  const csv = '\uFEFF' + rows.map(r => r.map(c => `"${String(c).replace(/"/g, '""')}"`).join(',')).join('\n');
  download(`log-records-${stamp()}.csv`, csv, 'text/csv;charset=utf-8');
  log(`已导出 CSV 明细（${list.length} 条，字段：${cols.join(',')}）`);
}

/* ================= 会话持久化 ================= */
function saveSession() {
  chrome.storage.local.set({
    records: [...seen.values()],
    meta: { queryCount: state.queryCount, ruleId: state.activeRuleId, savedAt: Date.now() }
  });
}

async function clearSession() {
  if (state.running) { alert('采集进行中，请先停止'); return; }
  if (seen.size && !confirm(`确认清空已采集的 ${seen.size} 条记录？`)) return;
  seen.clear();
  state.queryCount = 0;
  state.truncatedCount = 0;
  state.failedCount = 0;
  chrome.storage.local.remove(['records', 'meta']);
  state.logs = [];
  refreshStats();
  log('已清空结果');
}

async function closeTabs() {
  const ids = [runState.tabId, runState.apiTabId].filter(x => x != null);
  for (const id of ids) { try { await chrome.tabs.remove(id); } catch (e) { /* ignore */ } }
  runState.tabId = null;
  runState.apiTabId = null;
  if (ids.length) log('已关闭查询标签页');
}

/* ================= 便捷操作 ================= */
function refreshApiTpl() {
  const api = toApiTemplate(state.tplUrl);
  if (api) state.apiTpl = api;
}

function applyQuickRange() {
  const v = state.quickRange;
  if (!v) return;
  const e = new Date();
  const b = new Date(e.getTime() - (+v) * 3600000);
  state.beginTime = dtToInput(b);
  state.endTime = dtToInput(e);
}

/* ================= 初始化 ================= */
async function init() {
  window.addEventListener('beforeunload', e => {
    if (state.running) { e.preventDefault(); e.returnValue = ''; }
  });

  const params = new URLSearchParams(location.search);
  const saved = await chrome.storage.local.get(
    ['tplUrl', 'apiTpl', 'mode', 'lineCnt', 'winSize', 'fullMode', 'offsetStep',
     'customRules', 'activeRuleId', 'records', 'meta']);

  // 解析规则：内置 + 自定义
  state.rules = [
    ...LogRules.BUILTIN_RULES.map(r => JSON.parse(JSON.stringify(r))),
    ...(saved.customRules || []),
  ];
  state.activeRuleId = (saved.activeRuleId && state.rules.some(r => r.id === saved.activeRuleId))
    ? saved.activeRuleId : state.rules[0].id;

  // 查询配置
  const tpl = params.get('tpl') || saved.tplUrl || '';
  if (tpl) {
    state.tplUrl = tpl;
    try {
      const u = new URL(tpl);
      const b = u.searchParams.get('begintime');
      const e = u.searchParams.get('endtime');
      if (b) state.beginTime = b.replace(' ', 'T').slice(0, 16);
      if (e) state.endTime = e.replace(' ', 'T').slice(0, 16);
    } catch (err) { /* ignore */ }
  }
  if (saved.apiTpl) state.apiTpl = saved.apiTpl;
  else refreshApiTpl();
  if (saved.mode) state.mode = saved.mode;
  if (saved.lineCnt != null) state.lineCnt = saved.lineCnt;
  if (saved.winSize) state.winSize = saved.winSize;
  if (saved.fullMode != null) state.fullMode = !!saved.fullMode;
  if (saved.offsetStep) state.offsetStep = saved.offsetStep;
  if (!state.beginTime || !state.endTime) applyQuickRange();

  // 恢复上次采集的记录
  if (saved.records && saved.records.length) {
    const rule = activeRuleRaw();
    for (const r of saved.records) seen.set(LogRules.recordKey(r, rule), r);
    state.queryCount = (saved.meta && saved.meta.queryCount) || 0;
    log(`已载入上次会话的 ${saved.records.length} 条记录（点击「清空结果」可重置）`, 'warn');
    refreshStats();
  }
}

/* ============================================================
 * 渲染（h() 渲染函数；组件拆分以缩小重渲染范围）
 * ============================================================ */
const V = {
  panel(title, sub, kids) {
    return h('section', { class: 'panel' }, [
      h('h2', {}, [title, sub ? h('span', { class: 'sub' }, sub) : null]),
      ...kids,
    ]);
  },
  txt(label, get, set, attrs = {}) {
    return h('div', {}, [
      h('label', {}, label),
      h('input', Object.assign({ value: get(), onInput: e => set(e.target.value) }, attrs)),
    ]);
  },
  num(label, get, set, attrs = {}) {
    return h('div', {}, [
      h('label', {}, label),
      h('input', Object.assign({
        type: 'number',
        value: String(get()),
        onInput: e => set(parseInt(e.target.value, 10) || 0),
      }, attrs)),
    ]);
  },
  sel(label, get, set, options) {
    return h('div', {}, [
      h('label', {}, label),
      h('select', { value: get(), onChange: e => set(e.target.value) },
        options.map(o => h('option', { key: o.value, value: o.value }, o.label))),
    ]);
  },
  check(label, get, set) {
    return h('label', { class: 'check' }, [
      h('input', { type: 'checkbox', checked: get(), onChange: e => set(e.target.checked) }),
      h('span', {}, label),
    ]);
  },
  btn(label, onClick, cls = '', disabled = false) {
    return h('button', { class: cls, disabled: disabled || undefined, onClick }, label);
  },
  bar(ratio, color) {
    return h('div', { class: 'bar-wrap' }, [
      h('div', {
        class: 'bar',
        style: { width: Math.min(ratio * 100, 100).toFixed(2) + '%', background: color },
      }),
    ]);
  },
  dot(color) {
    return h('span', { class: 'dot', style: { background: color } });
  },
};

/* ---------- 查询配置面板 ---------- */
const ConfigPanel = {
  setup() {
    return () => V.panel('查询配置', null, [
      h('label', {}, '日志查询页 URL（logsearch.html 或直接粘贴 DevTools 里的 showlog/query 请求 URL）'),
      h('textarea', {
        rows: 3,
        value: state.tplUrl,
        placeholder: 'https://log.wwitil.woa.com/logsearch.html?ip=...&keywords=...&page=query&rows=10000',
        onInput: e => { state.tplUrl = e.target.value; refreshApiTpl(); },
      }),
      h('div', { class: 'field' }, [
        h('label', {}, '查询接口 URL（自动从上方转换；可手动粘贴 DevTools Network 中 showlog/query 的请求 URL 修正）'),
        h('input', {
          value: state.apiTpl,
          placeholder: 'https://log.wwitil.woa.com/showlog/query?page=query&keywords=...&type=2&...',
          onInput: e => { state.apiTpl = e.target.value; },
        }),
      ]),
      h('div', { class: 'row' }, [
        V.sel('快捷时间范围', () => state.quickRange,
          v => { state.quickRange = v; applyQuickRange(); },
          [
            { value: '', label: '自定义' },
            { value: '1', label: '最近 1 小时' },
            { value: '3', label: '最近 3 小时' },
            { value: '6', label: '最近 6 小时' },
            { value: '12', label: '最近 12 小时' },
            { value: '24', label: '最近 24 小时' },
          ]),
        V.txt('开始时间', () => state.beginTime,
          v => { state.beginTime = v; state.quickRange = ''; }, { type: 'datetime-local' }),
        V.txt('结束时间', () => state.endTime,
          v => { state.endTime = v; state.quickRange = ''; }, { type: 'datetime-local' }),
        V.sel('采集模式', () => state.mode, v => { state.mode = v; }, [
          { value: 'api', label: '接口直连（推荐）' },
          { value: 'page', label: '页面抓取（兼容）' },
        ]),
      ]),
      h('div', { class: 'row' }, [
        V.num('窗口大小（分钟）', () => state.winSize, v => { state.winSize = v; }, { min: 1 }),
        V.num('每页行数 line_cnt（0=默认约2000）', () => state.lineCnt, v => { state.lineCnt = v; }, { min: 0 }),
        V.num('offset 步长（翻页）', () => state.offsetStep, v => { state.offsetStep = v; }, { min: 1 }),
        V.num('最小二分窗口（分钟）', () => state.minWin, v => { state.minWin = v; }, { min: 1 }),
        V.num('最大请求次数', () => state.maxQ, v => { state.maxQ = v; }, { min: 1, max: 1000 }),
      ]),
      h('div', { class: 'field' }, [
        V.check('窗口内分页拉全：截断时按 offset 递增翻页直至拉完（同 continueQuery）；不勾选则每窗口仅取第一页（默认约 2000 条，可能不足）',
          () => state.fullMode, v => { state.fullMode = v; }),
      ]),
      h('div', { class: 'btns' }, [
        V.btn('开始收集', start, 'primary', state.running),
        V.btn('停止', stop, '', !state.running),
        V.btn('清空结果', clearSession, '', state.running),
        V.btn('关闭查询标签页', closeTabs, '', false),
        h('span', { class: 'status' }, state.statusText),
      ]),
      h('div', { class: 'hint' },
        '说明：需要一个已登录的 log.wwitil.woa.com 标签页承载接口请求（自动查找，没有会自动打开）；' +
        '记录按规则的去重字段去重；翻页优先使用响应中的游标字段（next_offset/cursor），无游标则按「offset 步长」递增，' +
        '翻页无新数据时自动二分时间窗兜底。采集期间保持本页与查询标签页开启。停止或误关页面后数据自动保留。'),
    ]);
  },
};

/* ---------- 解析规则面板 ---------- */
const RulesPanel = {
  setup() {
    return () => {
      const rule = activeRuleRaw();
      return V.panel('解析规则',
        `${state.rules.length} 套规则 · 当前：${rule ? rule.name : '无'}`, [
        h('div', { class: 'row' }, [
          V.sel('当前规则', () => state.activeRuleId, setRule,
            state.rules.map(r => ({ value: r.id, label: r.name }))),
          h('div', { style: { alignSelf: 'end' } }, [
            V.btn('新建规则', () => openEditor(null)),
          ]),
          h('div', { class: 'rule-desc' }, rule ? (rule.description || '') : ''),
        ]),
        h('table', {}, [
          h('thead', {}, [h('tr', {},
            ['名称', '类型', '行过滤', '统计维度', '去重字段', '操作'].map(x => h('th', {}, x)))]),
          h('tbody', {}, state.rules.map(r => h('tr', { key: r.id }, [
            h('td', {}, [h('b', {}, r.name)]),
            h('td', {}, [h('span', { class: 'tag ' + (r.builtin ? 'builtin' : 'custom') },
              r.builtin ? '内置' : '自定义')]),
            h('td', {}, [h('code', {}, r.lineMatch || '—')]),
            h('td', {}, (r.dimensions || []).join(' → ') || '—'),
            h('td', {}, r.dedupeBy || '—'),
            h('td', { class: 'rule-actions' }, [
              ...(r.id !== state.activeRuleId
                ? [V.btn('使用', () => setRule(r.id), 'sm')] : []),
              V.btn(r.builtin ? '复制' : '编辑', () => openEditor(r), 'sm'),
              ...(r.builtin ? [] : [V.btn('删除', () => deleteRule(r.id), 'sm')]),
            ]),
          ]))),
        ]),
        state.editorOpen ? renderEditor() : null,
      ]);
    };
  },
};

function renderEditor() {
  const d = state.draft;
  if (!d) return null;
  const fieldOpts = [
    { value: '', label: '(无)' },
    ...d.fields.filter(f => f.name.trim()).map(f => ({ value: f.name, label: f.name })),
  ];
  return h('div', { class: 'editor' }, [
    h('h3', {}, '规则编辑' + (d.id ? '' : '（新建）') +
      (d.id ? '' : ' —— 字段正则请用捕获组，如 [?&]platform=([^&\\s]+)')),
    h('div', { class: 'row' }, [
      V.txt('规则名称', () => d.name, v => { d.name = v; }, { placeholder: '如：错误码统计' }),
      V.txt('行过滤（包含该子串的行才解析，可留空）', () => d.lineMatch, v => { d.lineMatch = v; },
        { placeholder: '如 platform=' }),
      V.txt('截断标记正则（可留空）', () => d.truncation, v => { d.truncation = v; },
        { placeholder: 'InCompleteResult|部分日志因超出行数限制' }),
    ]),
    h('label', { style: { marginTop: '12px' } },
      '字段提取（正则，第 1 个捕获组为字段值；无捕获组则取整段匹配）'),
    d.fields.map((f, i) => h('div', { class: 'fields-row', key: i }, [
      h('input', { value: f.name, placeholder: '字段名，如 platform',
        onInput: e => { f.name = e.target.value; } }),
      h('input', { value: f.re, placeholder: '正则，如 [?&]platform=([^&\\s]+)',
        onInput: e => { f.re = e.target.value; } }),
      V.btn('×', () => d.fields.splice(i, 1), 'sm', d.fields.length <= 1),
    ])),
    V.btn('+ 添加字段', () => d.fields.push({ name: '', re: '' }), 'sm'),
    h('div', { class: 'row' }, [
      V.sel('主统计维度', () => d.dims[0], v => { d.dims[0] = v; }, fieldOpts),
      V.sel('次统计维度（可选）', () => d.dims[1], v => { d.dims[1] = v; }, fieldOpts),
      V.sel('设备字段（可选）', () => d.deviceField, v => { d.deviceField = v; }, fieldOpts),
      V.sel('去重字段（可选）', () => d.dedupeBy, v => { d.dedupeBy = v; }, fieldOpts),
    ]),
    h('div', { class: 'btns' }, [
      V.btn('保存规则', saveDraft, 'primary'),
      V.btn('取消', () => { state.editorOpen = false; }),
    ]),
  ]);
}

/* ---------- 采集进度面板 ---------- */
const LogList = {
  setup() {
    const elRef = ref(null);
    watch(() => state.logs.length, () => nextTick(() => {
      if (elRef.value) elRef.value.scrollTop = elRef.value.scrollHeight;
    }));
    return () => h('div', { class: 'loglist', ref: elRef },
      state.logs.map(l => h('div', { class: 'logline ' + (l.cls || ''), key: l.key }, l.msg)));
  },
};

const ProgressPanel = {
  components: { LogList },
  setup() {
    return () => V.panel('采集进度',
      `${state.queryCount} 次请求 · ${state.recordCount} 条记录 · 截断页 ${state.truncatedCount} · 失败 ${state.failedCount}`,
      [h(LogList)]);
  },
};

/* ---------- 统计结果面板 ---------- */
function statCards(st, groups) {
  const d1 = st.d1 || '维度';
  return h('div', { class: 'cards' }, [
    h('div', { class: 'card' }, [h('div', { class: 'card-num' }, st.total.toLocaleString()), h('div', { class: 'card-label' }, '有效记录')]),
    h('div', { class: 'card' }, [h('div', { class: 'card-num' }, String(groups.length)), h('div', { class: 'card-label' }, `${d1} 取值数`)]),
    ...(st.devF ? [h('div', { class: 'card' }, [h('div', { class: 'card-num' }, st.allDev.toLocaleString()), h('div', { class: 'card-label' }, '独立设备')])] : []),
    h('div', { class: 'card' }, [h('div', { class: 'card-num' }, String(state.queryCount)), h('div', { class: 'card-label' }, '请求次数')]),
  ]);
}

const StatsPanel = {
  setup() {
    return () => {
      const rule = activeRuleRaw();
      let body;
      const st = state.stats;
      if (!st || !st.total) {
        body = [h('div', { class: 'hint' },
          '暂无数据。点击「开始收集」后，这里会按当前解析规则（' + (rule ? rule.name : '无') + '）展示统计结果。')];
      } else {
        const groups = [...st.groups.entries()].sort((a, b) => b[1].count - a[1].count);
        const colors = {};
        groups.forEach(([k], i) => colors[k] = PLATFORM_COLORS[i % PLATFORM_COLORS.length]);
        const d1 = st.d1 || '维度';
        const d2 = st.d2;
        const hasDev = !!st.devF;

        const mainHead = [d1, '记录数', '占比', '分布',
          ...(hasDev ? ['独立设备'] : []), ...(d2 ? [`${d2} 数`] : [])];
        const mainTable = h('table', {}, [
          h('thead', {}, [h('tr', {}, mainHead.map(x => h('th', {}, x)))]),
          h('tbody', {}, groups.map(([k, g]) => h('tr', { key: k }, [
            h('td', {}, [V.dot(colors[k]), h('b', {}, k)]),
            h('td', { class: 'num' }, g.count.toLocaleString()),
            h('td', { class: 'num' }, [h('b', {}, pct(g.count, st.total).toFixed(2) + '%')]),
            h('td', {}, [V.bar(g.count / st.total, colors[k])]),
            ...(hasDev ? [h('td', { class: 'num' }, g.dev.size.toLocaleString())] : []),
            ...(d2 ? [h('td', { class: 'num' }, String(g.sub.size))] : []),
          ]))),
        ]);

        const subs = d2 ? groups.map(([k, g]) => {
          const subList = [...g.sub.entries()].sort((a, b) => b[1] - a[1]);
          return h('div', { key: 'g-' + k }, [
            h('h3', {}, [
              V.dot(colors[k]), k,
              h('span', { class: 'sub' },
                `${g.count.toLocaleString()} 条 · ${g.sub.size} 个取值 · 占全部 ${pct(g.count, st.total).toFixed(2)}%`),
            ]),
            h('table', {}, [
              h('thead', {}, [h('tr', {},
                [d2, '记录数', '组内占比', '分布', ...(hasDev ? ['独立设备'] : [])].map(x => h('th', {}, x)))]),
              h('tbody', {}, subList.map(([v, c]) => h('tr', { key: v }, [
                h('td', {}, [h('code', {}, v)]),
                h('td', { class: 'num' }, c.toLocaleString()),
                h('td', { class: 'num' }, [h('b', {}, pct(c, g.count).toFixed(2) + '%')]),
                h('td', {}, [V.bar(c / g.count, colors[k])]),
                ...(hasDev ? [h('td', { class: 'num' },
                  ((g.subDev.get(v) || new Set()).size).toLocaleString())] : []),
              ]))),
            ]),
          ]);
        }) : [];

        body = [
          statCards(st, groups),
          h('h3', {}, `${d1} 占比`),
          mainTable,
          ...subs,
        ];
      }

      return V.panel('统计结果 · ' + (rule ? rule.name : ''), null, [
        ...body,
        h('div', { class: 'btns' }, [
          V.btn('导出 HTML 报告', exportHtml, 'primary', !state.recordCount),
          V.btn('导出 CSV 明细', exportCsv, '', !state.recordCount),
        ]),
      ]);
    };
  },
};

/* ---------- 根组件 ---------- */
const App = {
  components: { ConfigPanel, RulesPanel, ProgressPanel, StatsPanel },
  setup() {
    onMounted(init);
    return () => h('div', { class: 'container' }, [
      h('header', {}, [
        h('h1', {}, '日志自循环收集分析器'),
        h('div', { class: 'meta' },
          '接口直连 + 时间窗循环采集 + 多套可配置解析规则（内置「平台和版本统计」「来源 IP 统计」，可新建自定义规则适配其他日志）。'),
      ]),
      h(ConfigPanel),
      h(RulesPanel),
      h(ProgressPanel),
      h(StatsPanel),
    ]);
  },
};

createApp(App).mount('#app');
