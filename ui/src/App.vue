<script setup>
import { ref, computed, watch, nextTick, onMounted, onBeforeUnmount } from "vue";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const items = ref([]);
const activeIndex = ref(0);
const query = ref("");
const searchInput = ref(null);
const listEl = ref(null);
const titlebarDragging = ref(false);
// R19 多选状态机：selected=已选 id 集合，anchorIndex=Shift 扩展锚点（null=无锚点）
const selected = ref(new Set());
const anchorIndex = ref(null);

// ---------- 右侧详情面板 ----------

const detail = ref(null);
const activeItem = computed(() => items.value[activeIndex.value] || null);
// 异步防串扰：快速移动高亮时只保留最后一次请求的结果
let detailSeq = 0;
async function loadDetail() {
  const it = activeItem.value;
  if (!it) {
    detail.value = null;
    return;
  }
  const seq = ++detailSeq;
  try {
    const d = await invoke("get_item_detail", { id: it.id });
    if (seq === detailSeq) detail.value = d;
  } catch (e) {
    if (seq === detailSeq) detail.value = null;
  }
}
// activeIndex（键盘/鼠标高亮）与 items（刷新/重排）任一变化都重新拉详情
watch([activeIndex, items], loadDetail, { immediate: true });

function fmtBytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(2)} MB`;
}

function fmtTime(ts) {
  const d = new Date(ts);
  const p = (x) => String(x).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

async function pasteDetail() {
  if (detail.value) select(detail.value.id);
}

async function copyDetail() {
  if (!detail.value) return;
  try {
    await invoke("copy_item", { id: detail.value.id });
  } catch (e) {
    console.error(e);
  }
}

async function togglePinDetail() {
  if (!detail.value) return;
  await invoke("toggle_pin", { id: detail.value.id });
  await refresh(true);
}

async function deleteDetail() {
  if (!detail.value) return;
  await invoke("delete_item", { id: detail.value.id });
  await refresh();
}

function relTime(ts) {
  const diff = Date.now() - ts;
  if (diff < 60_000) return "刚刚";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
  return `${Math.floor(diff / 86_400_000)} 天前`;
}

function metaOf(it) {
  if (it.type === "image") return `${relTime(it.time)} · ${it.width}×${it.height}`;
  const chars = [...(it.text || "")].length;
  return relTime(it.time) + (chars > 300 ? ` · ${chars} 字符` : "");
}

function scrollActiveIntoView() {
  nextTick(() => {
    const el = listEl.value?.children[activeIndex.value];
    el?.scrollIntoView({ block: "nearest" });
  });
}

function setActive(i) {
  if (i === activeIndex.value) return;
  activeIndex.value = i;
  scrollActiveIntoView();
}

// ---------- data ----------

async function refresh(keepActive = false) {
  items.value = await invoke("get_history", { query: query.value });
  if (!keepActive || activeIndex.value >= items.value.length) activeIndex.value = 0;
  // R21 P2-1：列表重排/过滤后，剔除已不在新列表中的选中 id（仍存在的选中项保留）；
  // anchor 越界则清空，Shift 扩展时回退到从当前高亮开始
  if (selected.value.size) {
    const visible = new Set(items.value.map((it) => it.id));
    const pruned = [...selected.value].filter((id) => visible.has(id));
    if (pruned.length !== selected.value.size) selected.value = new Set(pruned);
  }
  if (anchorIndex.value != null && anchorIndex.value >= items.value.length) {
    anchorIndex.value = null;
  }
  scrollActiveIntoView();
}

async function select(id) {
  try {
    await invoke("select_item", { id });
  } catch (e) {
    console.error(e);
  }
}

// ---------- R19 多选 ----------

function clearSelection() {
  if (selected.value.size) selected.value = new Set();
}

function toggleSelected(id) {
  const s = new Set(selected.value);
  if (s.has(id)) s.delete(id);
  else s.add(id);
  selected.value = s;
}

// 按当前列表显示顺序取选中 id（batch 粘贴按此顺序拼接）
function selectedIdsInOrder() {
  return items.value.filter((it) => selected.value.has(it.id)).map((it) => it.id);
}

async function batchSelect() {
  const ids = selectedIdsInOrder();
  clearSelection();
  try {
    await invoke("batch_select", { ids });
  } catch (e) {
    console.error(e);
  }
}

async function batchDelete() {
  const ids = selectedIdsInOrder();
  clearSelection();
  await invoke("batch_delete", { ids });
  await refresh();
}

// Shift+↑/↓：从锚点到当前高亮项的连续选区
function extendSelection(next) {
  // 无锚点（refresh 越界清空后）或无现有多选时，从当前高亮起锚
  if (anchorIndex.value == null || !selected.value.size) anchorIndex.value = activeIndex.value;
  const lo = Math.min(anchorIndex.value, next);
  const hi = Math.max(anchorIndex.value, next);
  selected.value = new Set(items.value.slice(lo, hi + 1).map((it) => it.id));
}

function onItemClick(e, it, i) {
  if (e.metaKey || e.ctrlKey) {
    activeIndex.value = i;
    anchorIndex.value = i;
    toggleSelected(it.id);
  } else {
    clearSelection();
    select(it.id);
  }
}

async function togglePin(it) {
  await invoke("toggle_pin", { id: it.id });
  await refresh(true);
}

async function deleteItem(it) {
  await invoke("delete_item", { id: it.id });
  await refresh();
}

async function clearAll() {
  await invoke("clear_history");
  await refresh();
}

// ---------- 搜索防抖：停顿 250ms 才触发；IME 组合中（中文输入）不触发 ----------

let searchTimer = null;
let composing = false;
function onQueryInput() {
  if (composing) return;
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => refresh(), 250);
}
function onCompositionStart() {
  composing = true;
}
function onCompositionEnd() {
  composing = false;
  onQueryInput();
}

// ---------- keyboard / focus ----------

function onKeydown(e) {
  const n = items.value.length;
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    if (!n) return;
    const dir = e.key === "ArrowDown" ? 1 : -1;
    if (e.shiftKey) {
      // Shift：anchor 扩展连续选区
      const next = Math.min(Math.max(activeIndex.value + dir, 0), n - 1);
      extendSelection(next);
      setActive(next);
    } else if (e.metaKey || e.ctrlKey) {
      // Cmd+↑/↓：切换当前项选中状态（不移动高亮）
      const it = items.value[activeIndex.value];
      if (it) toggleSelected(it.id);
    } else {
      // 单选导航：清空多选
      clearSelection();
      setActive(Math.min(Math.max(activeIndex.value + dir, 0), n - 1));
    }
  } else if (e.key === "Enter") {
    e.preventDefault();
    if (selected.value.size > 0) {
      batchSelect();
    } else {
      const it = items.value[activeIndex.value];
      if (it) select(it.id);
    }
  } else if (e.key === "Delete" || e.key === "Backspace") {
    // R21 P2-2：搜索框聚焦时 Backspace 永远只编辑文本（query 删空后也不落到删除分支），
    // 删除条目仅在搜索框无焦点时可用（Delete 键不受此限）
    if (e.key === "Backspace" && document.activeElement === searchInput.value) return;
    e.preventDefault();
    if (selected.value.size > 0) {
      batchDelete();
    } else {
      const it = items.value[activeIndex.value];
      if (it) deleteItem(it);
    }
  } else if (e.key === "Escape") {
    e.preventDefault();
    // 先清多选；无多选才关面板
    if (selected.value.size > 0) clearSelection();
    else invoke("hide_panel");
  } else if ((e.metaKey || e.ctrlKey) && (e.key === "p" || e.key === "P")) {
    e.preventDefault();
    const it = items.value[activeIndex.value];
    if (it) togglePin(it);
  }
}

// 面板 resign key（用户点击了其他应用）时自动隐藏
let blurHideTimer = null;
function onBlur() {
  clearTimeout(blurHideTimer);
  blurHideTimer = setTimeout(() => {
    // 拖拽期间不隐藏（面板被拖动时失焦属正常）
    if (!document.hasFocus() && !titlebarDragging.value) invoke("hide_panel");
  }, 150);
}
function onFocus() {
  clearTimeout(blurHideTimer);
}

// 面板重新显示时把列表滚回顶部：隐藏期间 DOM 存活，上次的 scrollTop 残留在中间；
// scrollIntoView 在 webview 刚恢复渲染时可能被忽略（与 DOM focus 失败同理），
// 直接写 scrollTop 并延迟重试兜底
function resetListScroll(retry = 0) {
  if (listEl.value) {
    // 强制 reflow：transparent 窗口 orderOut→makeKeyAndOrderFront 回来时 WKWebView
    // 可能缓存了旧的合成层（导致 .item 存在但不绘制），读 offsetHeight 触发同步布局
    void listEl.value.offsetHeight;
    listEl.value.scrollTop = 0;
  }
  if (retry > 0) setTimeout(() => resetListScroll(retry - 1), 50);
}

// 兜底：NSPanel hidden→shown 时 WKWebView 滚动层的 tile 缓存未刷新，
// 导致 .list 区域不绘制（实测：forceListRepaint 仅靠 transform 切换不够，
// 整面板甚至会变成空白窗口）。display: none 切换彻底销毁并重建滚动容器，
// 配合 scrollTop 先到底再回 0 强制新 tile 加载，这是最暴力的失效策略。
async function forceListRepaint() {
  const el = listEl.value;
  if (!el) return;
  // 1) 先把滚动位置打到极限，触发完整 tile 计算
  const prevTop = el.scrollTop;
  el.scrollTop = el.scrollHeight;
  // 2) 切换 display 销毁滚动层
  el.style.display = "none";
  // 同步读 offsetHeight 触发 layout
  void el.offsetHeight;
  // 3) 恢复 display，让浏览器重新创建滚动层并重新加载所有 tile
  el.style.display = "";
  await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  // 4) 复位 scrollTop 到 0（panel-shown 监听后续还会再 set，但这里双保险）
  el.scrollTop = 0;
}

// 面板刚显示时 webview 可能尚未真正成 key，DOM focus 会失败，重试几次
function focusSearch(retry = 0) {
  searchInput.value?.focus();
  if (document.activeElement !== searchInput.value && retry > 0) {
    setTimeout(() => focusSearch(retry - 1), 50);
  }
}

// ---------- titlebar drag（手工追踪：nonactivating NSPanel 上 startDragging 无效） ----------

let dragRafId = 0;
let dragLastEvt = null;
function onTitlebarMousedown(e) {
  if (e.button !== 0) return;
  titlebarDragging.value = true;
  invoke("drag_begin", { x: e.screenX, y: e.screenY });
  // 监听挂在 document 上：窗口跟随鼠标移动时相对位置不变，
  // 但若 invoke 延迟导致窗口落后于鼠标，titlebar 自身的 mousemove/mouseup 会断
  document.addEventListener("mousemove", onDragMousemove);
  document.addEventListener("mouseup", stopDragging, { once: true });
}
function onDragMousemove(e) {
  dragLastEvt = e;
  if (dragRafId) return; // rAF 节流：每帧最多一次 invoke
  dragRafId = requestAnimationFrame(() => {
    dragRafId = 0;
    if (dragLastEvt) {
      invoke("drag_move", { x: dragLastEvt.screenX, y: dragLastEvt.screenY });
    }
  });
}
function stopDragging() {
  if (!titlebarDragging.value) return;
  titlebarDragging.value = false;
  document.removeEventListener("mousemove", onDragMousemove);
  if (dragRafId) {
    cancelAnimationFrame(dragRafId);
    dragRafId = 0;
  }
  dragLastEvt = null;
  invoke("drag_end");
}

// ---------- lifecycle ----------

let unlisteners = [];
onMounted(async () => {
  document.addEventListener("keydown", onKeydown);
  window.addEventListener("blur", onBlur);
  window.addEventListener("focus", onFocus);

  // 主题：启动读取 settings.json 应用；监听菜单栏切换事件同步
  try {
    document.body.dataset.theme = await invoke("get_theme");
  } catch (e) {
    console.error(e);
  }
  unlisteners.push(
    await listen("theme-changed", (ev) => {
      document.body.dataset.theme = ev.payload;
    }),
  );

  unlisteners.push(await listen("history-changed", () => refresh(true)));
  unlisteners.push(
    await listen("panel-shown", async () => {
      query.value = "";
      activeIndex.value = 0;
      clearSelection();
      await refresh();
      resetListScroll(4);
      await forceListRepaint(); // 兜底 WKWebView 合成层缓存导致左侧不绘制
      focusSearch(4);
    }),
  );

  await refresh();
});
onBeforeUnmount(() => {
  clearTimeout(searchTimer);
  document.removeEventListener("keydown", onKeydown);
  window.removeEventListener("blur", onBlur);
  window.removeEventListener("focus", onFocus);
  unlisteners.forEach((un) => un());
});
</script>

<template>
  <div class="panel">
    <div
      class="titlebar"
      :class="{ dragging: titlebarDragging }"
      @mousedown="onTitlebarMousedown"
    >
      ClipMate
    </div>

    <div class="header">
      <input
        id="search"
        ref="searchInput"
        v-model="query"
        type="text"
        placeholder="搜索剪贴板历史…"
        autocomplete="off"
        spellcheck="false"
        @input="onQueryInput"
        @compositionstart="onCompositionStart"
        @compositionend="onCompositionEnd"
      />
      <button class="icon-btn" title="清空历史" @click="clearAll">⌫</button>
    </div>

    <div class="body-row">
      <div class="list-side">
        <div v-show="items.length" ref="listEl" class="list">
          <div
            v-for="(it, i) in items"
            :key="it.id"
            class="item"
            :class="{ active: i === activeIndex, pinned: it.pinned, selected: selected.has(it.id) }"
            @click="onItemClick($event, it, i)"
            @mouseenter="setActive(i)"
          >
            <div class="item-icon">
              <span v-if="selected.has(it.id)" class="item-check">✓</span>
              <template v-else>
                <img v-if="it.type === 'image'" :src="it.image" alt="" />
                <template v-else>T</template>
              </template>
            </div>
            <div class="item-body">
              <div class="item-text">{{ it.type === "image" ? "图片" : it.text }}</div>
              <div class="item-meta">{{ metaOf(it) }}</div>
            </div>
            <button
              class="item-pin"
              :title="it.pinned ? '取消置顶' : '置顶该项（⌘P）'"
              @click.stop="togglePin(it)"
            >
              {{ it.pinned ? "★" : "☆" }}
            </button>
            <button class="item-del" title="删除该记录" @click.stop="deleteItem(it)">✕</button>
          </div>
        </div>

        <div v-show="!items.length" class="empty">
          <div class="empty-icon">📋</div>
          <div>暂无复制记录</div>
          <div class="empty-sub">复制点内容，然后按 F2 唤起</div>
        </div>
      </div>

      <div v-if="items.length" class="detail">
        <template v-if="detail">
          <div class="detail-head">
            <span class="detail-badge">{{ detail.type === "image" ? "图片" : "文本" }}</span>
            <span v-if="detail.pinned" class="detail-pinned">★ 已置顶</span>
          </div>

          <div class="detail-content">
            <img v-if="detail.type === 'image'" class="detail-image" :src="detail.image" alt="" />
            <pre v-else class="detail-text">{{ detail.text }}</pre>
          </div>

          <div class="detail-meta">
            <div v-if="detail.type === 'text'" class="meta-row">
              <span>字符数</span><span>{{ detail.chars }}</span>
            </div>
            <div v-if="detail.type === 'text'" class="meta-row">
              <span>行数</span><span>{{ detail.lines }}</span>
            </div>
            <div v-if="detail.type === 'image'" class="meta-row">
              <span>尺寸</span><span>{{ detail.width }} × {{ detail.height }}</span>
            </div>
            <div class="meta-row">
              <span>大小</span><span>{{ fmtBytes(detail.bytes) }}</span>
            </div>
            <div class="meta-row">
              <span>复制时间</span><span>{{ fmtTime(detail.time) }}</span>
            </div>
          </div>

          <div class="detail-actions">
            <button class="action-btn primary" @click="pasteDetail">粘贴</button>
            <button class="action-btn" @click="copyDetail">复制</button>
            <button class="action-btn" @click="togglePinDetail">
              {{ detail.pinned ? "取消置顶" : "置顶" }}
            </button>
            <button class="action-btn danger" @click="deleteDetail">删除</button>
          </div>
        </template>
      </div>
    </div>

    <div class="footer">
      <span>↑↓ 选择</span><span>⇧/⌘ 多选</span><span>⏎ 粘贴</span><span>⌘P 置顶</span><span>Esc 关闭</span><span>F2 切换</span>
    </div>
  </div>
</template>
