<script setup>
import { ref, nextTick, onMounted, onBeforeUnmount } from "vue";

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
      focusSearch(4);
    }),
  );

  await refresh();
});
onBeforeUnmount(() => {
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
        @input="refresh()"
      />
      <button class="icon-btn" title="清空历史" @click="clearAll">⌫</button>
    </div>

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

    <div class="footer">
      <span>↑↓ 选择</span><span>⇧/⌘ 多选</span><span>⏎ 粘贴</span><span>⌘P 置顶</span><span>Esc 关闭</span><span>F2 切换</span>
    </div>
  </div>
</template>
