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
  scrollActiveIntoView();
}

async function select(id) {
  try {
    await invoke("select_item", { id });
  } catch (e) {
    console.error(e);
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
  if (e.key === "ArrowDown") {
    e.preventDefault();
    setActive(Math.min(activeIndex.value + 1, items.value.length - 1));
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    setActive(Math.max(activeIndex.value - 1, 0));
  } else if (e.key === "Enter") {
    e.preventDefault();
    const it = items.value[activeIndex.value];
    if (it) select(it.id);
  } else if (e.key === "Escape") {
    e.preventDefault();
    invoke("hide_panel");
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
    if (!document.hasFocus()) invoke("hide_panel");
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

// ---------- titlebar drag ----------

const currentWin = window.__TAURI__?.window?.getCurrentWindow?.();
function onTitlebarMousedown(e) {
  if (e.button !== 0) return;
  titlebarDragging.value = true;
  currentWin?.startDragging?.();
}
function stopDragging() {
  titlebarDragging.value = false;
}

// ---------- lifecycle ----------

let unlisteners = [];
onMounted(async () => {
  document.addEventListener("keydown", onKeydown);
  window.addEventListener("blur", onBlur);
  window.addEventListener("focus", onFocus);

  unlisteners.push(await listen("history-changed", () => refresh(true)));
  unlisteners.push(
    await listen("panel-shown", async () => {
      query.value = "";
      activeIndex.value = 0;
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
      @mouseup="stopDragging"
      @mouseleave="stopDragging"
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
        :class="{ active: i === activeIndex, pinned: it.pinned }"
        @click="select(it.id)"
        @mouseenter="setActive(i)"
      >
        <div class="item-icon">
          <img v-if="it.type === 'image'" :src="it.image" alt="" />
          <template v-else>T</template>
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
      <span>↑↓ 选择</span><span>⏎ 粘贴</span><span>⌘P 置顶</span><span>Esc 关闭</span><span>F2 切换</span>
    </div>
  </div>
</template>
