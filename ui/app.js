const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const searchInput = document.getElementById("search");
const listEl = document.getElementById("list");
const emptyEl = document.getElementById("empty");

let items = [];
let activeIndex = 0;

// ---------- rendering ----------

function relTime(ts) {
  const diff = Date.now() - ts;
  if (diff < 60_000) return "刚刚";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
  return `${Math.floor(diff / 86_400_000)} 天前`;
}

function render() {
  listEl.innerHTML = "";
  emptyEl.hidden = items.length > 0;
  if (!items.length) return;

  const frag = document.createDocumentFragment();
  items.forEach((it, i) => {
    const row = document.createElement("div");
    row.className = "item" + (i === activeIndex ? " active" : "") + (it.pinned ? " pinned" : "");

    const icon = document.createElement("div");
    icon.className = "item-icon";
    let meta;
    if (it.type === "image") {
      const img = document.createElement("img");
      img.src = it.image;
      icon.appendChild(img);
      meta = `${relTime(it.time)} · ${it.width}×${it.height}`;
    } else {
      icon.textContent = "T";
      const chars = [...(it.text || "")].length;
      meta = relTime(it.time) + (chars > 300 ? ` · ${chars} 字符` : "");
    }

    const body = document.createElement("div");
    body.className = "item-body";
    if (it.type === "image") {
      const t = document.createElement("div");
      t.className = "item-text";
      t.textContent = "图片";
      body.appendChild(t);
    } else {
      const t = document.createElement("div");
      t.className = "item-text";
      t.textContent = it.text;
      body.appendChild(t);
    }
    const m = document.createElement("div");
    m.className = "item-meta";
    m.textContent = meta;
    body.appendChild(m);

    const pin = document.createElement("button");
    pin.className = "item-pin";
    pin.textContent = it.pinned ? "★" : "☆";
    pin.title = it.pinned ? "取消置顶" : "置顶该项（⌘P）";
    pin.addEventListener("click", async (e) => {
      e.stopPropagation();
      await invoke("toggle_pin", { id: it.id });
      await refresh(true);
    });

    const del = document.createElement("button");
    del.className = "item-del";
    del.textContent = "✕";
    del.title = "删除该记录";
    del.addEventListener("click", async (e) => {
      e.stopPropagation();
      await invoke("delete_item", { id: it.id });
      await refresh();
    });

    row.append(icon, body, pin, del);
    row.addEventListener("click", () => select(it.id));
    row.addEventListener("mouseenter", () => setActive(i));
    frag.appendChild(row);
  });
  listEl.appendChild(frag);
  scrollIntoView();
}

function setActive(i) {
  if (i === activeIndex) return;
  activeIndex = i;
  const rows = listEl.children;
  for (let r = 0; r < rows.length; r++) rows[r].classList.toggle("active", r === activeIndex);
  scrollIntoView();
}

function scrollIntoView() {
  const el = listEl.children[activeIndex];
  if (el) el.scrollIntoView({ block: "nearest" });
}

// ---------- data ----------

async function refresh(keepActive = false) {
  items = await invoke("get_history", { query: searchInput.value });
  if (!keepActive || activeIndex >= items.length) activeIndex = 0;
  render();
}

async function select(id) {
  try {
    await invoke("select_item", { id });
  } catch (e) {
    console.error(e);
  }
}

// 横幅只在粘贴失败时短暂出现；不做持续轮询（功能本身不依赖辅助权限）

// 非激活面板 resign key（用户点击了其他应用）时自动隐藏
let blurHideTimer = null;
window.addEventListener("blur", () => {
  clearTimeout(blurHideTimer);
  blurHideTimer = setTimeout(() => {
    if (!document.hasFocus()) invoke("hide_panel");
  }, 150);
});
window.addEventListener("focus", () => clearTimeout(blurHideTimer));

// Rust NSEvent monitor 兜底入口：window.__CLIPMATE_KEY__("up"/"down"/"select"/"hide")
window.__CLIPMATE_KEY__ = (action) => {
  if (action === "hide") {
    invoke("hide_panel");
  } else if (action === "up") {
    setActive(Math.max(activeIndex - 1, 0));
  } else if (action === "down") {
    setActive(Math.min(activeIndex + 1, items.length - 1));
  } else if (action === "select") {
    const it = items[activeIndex];
    if (it) select(it.id);
  } else if (action === "backspace") {
    searchInput.value = searchInput.value.slice(0, -1);
    refresh();
  }
};

// CGEventTap 注入的可打印字符（面板可见时按键不进前台应用，转给搜索框）
window.__CLIPMATE_CHAR__ = (ch) => {
  searchInput.value += ch;
  refresh();
};

// ---------- events ----------

searchInput.addEventListener("input", () => refresh());

document.addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown") {
    e.preventDefault();
    setActive(Math.min(activeIndex + 1, items.length - 1));
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    setActive(Math.max(activeIndex - 1, 0));
  } else if (e.key === "Enter") {
    e.preventDefault();
    const it = items[activeIndex];
    if (it) select(it.id);
  } else if (e.key === "Escape") {
    e.preventDefault();
    invoke("hide_panel");
  } else if ((e.metaKey || e.ctrlKey) && (e.key === "p" || e.key === "P")) {
    // R2: ⌘P 切换置顶
    e.preventDefault();
    const it = items[activeIndex];
    if (it) {
      invoke("toggle_pin", { id: it.id });
      refresh(true);
    }
  }
});

document.getElementById("clearBtn").addEventListener("click", async () => {
  await invoke("clear_history");
  await refresh();
});

listen("history-changed", () => refresh(true));
// 面板刚显示时 webview 可能尚未真正成 key，DOM focus 会失败
function focusSearch(retry) {
  searchInput.focus();
  if (document.activeElement !== searchInput && (retry || 0) > 0) {
    setTimeout(() => focusSearch(retry - 1), 50);
  }
}

listen("panel-shown", async () => {
  searchInput.value = "";
  activeIndex = 0;
  await refresh();
  focusSearch(4);
});

// initial load
refresh();
