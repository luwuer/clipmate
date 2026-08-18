//! 数据模型 + 纯逻辑（R7 从 main.rs 拆出，零行为变化）
//! - ItemKind / ClipboardItem / ItemDto / AppState
//! - 去重与上限策略（insert_dedup / enforce_limit）
//! - png 编解码与 DTO 转换

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use arboard::ImageData;
use serde::Serialize;

pub(crate) const MAX_ITEMS: usize = 300;
pub(crate) const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024; // 2 MB
pub(crate) const MAX_PNG_BYTES: usize = 8 * 1024 * 1024; // 8 MB

// ---------- data model ----------

#[derive(Clone)]
pub enum ItemKind {
    Text(String),
    Image { png: Vec<u8>, width: u32, height: u32 },
}

#[derive(Clone)]
pub struct ClipboardItem {
    pub(crate) id: u64,
    pub(crate) kind: ItemKind,
    pub(crate) created_at: u64, // unix ms
    pub(crate) pinned: bool,    // R2: 收藏/置顶，置顶项优先展示且防被新条目顶掉
}

#[derive(Serialize)]
pub struct ItemDto {
    pub(crate) id: u64,
    #[serde(rename = "type")]
    pub(crate) item_type: &'static str,
    pub(crate) text: Option<String>,
    pub(crate) image: Option<String>, // data url
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) time: u64,
    pub(crate) pinned: bool, // R2
}

pub struct AppState {
    pub(crate) items: Mutex<Vec<ClipboardItem>>,
    pub(crate) next_id: AtomicU64,
    pub(crate) last_change_count: AtomicI64,
    pub(crate) positioned: AtomicBool,
    pub(crate) shown_at: Mutex<Option<std::time::Instant>>,
    /// 唤起面板前处于前台的 App，作为 Cmd+V 的投递目标
    pub(crate) prev_front_pid: AtomicI32,
}

impl AppState {
    pub(crate) fn new() -> Self {
        Self {
            items: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            last_change_count: AtomicI64::new(i64::MIN),
            positioned: AtomicBool::new(false),
            shown_at: Mutex::new(None),
            prev_front_pid: AtomicI32::new(0),
        }
    }
}

// ---------- png helpers ----------

pub(crate) fn encode_png(img: &ImageData) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, img.width as u32, img.height as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(&img.bytes).ok()?;
    }
    Some(buf)
}

pub(crate) fn decode_png(bytes: &[u8]) -> Option<ImageData<'static>> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    buf.truncate(w as usize * h as usize * 4);
    Some(ImageData {
        width: w as usize,
        height: h as usize,
        bytes: Cow::Owned(buf),
    })
}

pub(crate) fn same_content(a: &ClipboardItem, b: &ClipboardItem) -> bool {
    match (&a.kind, &b.kind) {
        (ItemKind::Text(x), ItemKind::Text(y)) => x == y,
        (
            ItemKind::Image { png: p1, width: w1, height: h1 },
            ItemKind::Image { png: p2, width: w2, height: h2 },
        ) => p1 == p2 && w1 == w2 && h1 == h2,
        _ => false,
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 容量淘汰策略（R5，修 Critic P1）：
/// - 非 pinned 条目最多保留 MAX_ITEMS 条（从尾部淘汰最老的）
/// - **pinned 条目永不淘汰且不受上限约束**——数量由用户手动 pin 控制，
///   即使 pinned 数量本身超过 MAX_ITEMS 也全部保留
pub(crate) fn enforce_limit(items: &mut Vec<ClipboardItem>) {
    let mut unpinned_kept = 0usize;
    items.retain(|it| it.pinned || {
        unpinned_kept += 1;
        unpinned_kept <= MAX_ITEMS
    });
}

/// 去重与上限策略（R6，池 #8）：
/// 新条目若与现有**任意**条目内容相同：
/// - 命中的是 pinned → 不新增（pinned 已有该内容，且置顶分组本来就在最前）
/// - 命中的是非 pinned 旧条目 → 把旧条目提升到头部（CleanClip 式 recency 提升，
///   等价于"复制旧内容 = 置顶"），不新增条目
/// 无命中 → 头部新增并执行容量淘汰。
/// 返回 false 表示历史无变化（调用方跳过 save/emit）。
pub(crate) fn insert_dedup(items: &mut Vec<ClipboardItem>, new_item: ClipboardItem) -> bool {
    if let Some(pos) = items.iter().position(|it| same_content(it, &new_item)) {
        if items[pos].pinned {
            return false; // pinned 已有该内容
        }
        let mut old = items.remove(pos);
        // recency 提升关键：时间戳刷新为"最后复制时间"——get_history 按 created_at
        // 倒序渲染，不刷新则旧条目仍沉在后面，"复制旧内容=置顶"不成立
        old.created_at = new_item.created_at;
        items.insert(0, old); // 数量不变，无需再 enforce_limit
        true
    } else {
        items.insert(0, new_item);
        enforce_limit(items);
        true
    }
}

// ---------- tests (R5/R6) ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn text_item(id: u64, pinned: bool) -> ClipboardItem {
        ClipboardItem {
            id,
            kind: ItemKind::Text(format!("t{id}")),
            created_at: id,
            pinned,
        }
    }

    /// Critic P1 场景：满 300 条 + 尾部 1 条 pinned + 新条目进来 → pinned 存活
    #[test]
    fn pinned_survives_limit_eviction() {
        let mut items: Vec<ClipboardItem> =
            (0..MAX_ITEMS as u64).map(|i| text_item(i, false)).collect();
        items.push(text_item(999, true)); // pinned 沉到 Vec 尾部（最老位置）
        items.insert(0, text_item(1000, false)); // poller 新条目
        enforce_limit(&mut items);
        assert!(
            items.iter().any(|it| it.id == 999 && it.pinned),
            "pinned item must survive eviction"
        );
        assert_eq!(items.iter().filter(|it| !it.pinned).count(), MAX_ITEMS);
    }

    /// 非 pinned 上限仍受控：最新条目保留、总量封顶 MAX_ITEMS
    #[test]
    fn unpinned_capped_at_max_items() {
        let mut items: Vec<ClipboardItem> = (0..(MAX_ITEMS + 10) as u64)
            .map(|i| text_item(i, false))
            .collect();
        items.insert(0, text_item(9999, false));
        enforce_limit(&mut items);
        assert_eq!(items.len(), MAX_ITEMS);
        assert_eq!(items[0].id, 9999, "newest entry must be kept");
    }

    /// pinned 数量本身超过上限时全部保留（不受上限约束）
    #[test]
    fn pinned_over_limit_all_kept() {
        let mut items: Vec<ClipboardItem> = (0..(MAX_ITEMS + 5) as u64)
            .map(|i| text_item(i, true))
            .collect();
        items.insert(0, text_item(9999, false));
        enforce_limit(&mut items);
        assert_eq!(items.iter().filter(|it| it.pinned).count(), MAX_ITEMS + 5);
        assert_eq!(items.len(), MAX_ITEMS + 6);
    }

    // ---------- R6: 去重与上限策略统一 ----------

    /// 场景 1：新条目与任意 pinned 条目内容相同 → 不新增（返回 false、数量不变）
    #[test]
    fn dedup_pinned_match_not_inserted() {
        let mut items = vec![text_item(1, false), text_item(2, true), text_item(3, false)];
        let dup = ClipboardItem {
            id: 99,
            kind: ItemKind::Text("t2".into()), // 与 pinned 条目 id=2 同内容
            created_at: 99,
            pinned: false,
        };
        assert!(!insert_dedup(&mut items, dup), "pinned hit must be a no-op");
        assert_eq!(items.len(), 3, "no new entry");
        assert!(!items.iter().any(|it| it.id == 99));
        assert!(items.iter().any(|it| it.id == 2 && it.pinned), "pinned untouched");
    }

    /// 场景 2：新条目与非 pinned 旧条目相同 → 旧条目提升到头部，数量不变
    #[test]
    fn dedup_old_unpinned_promoted_to_head() {
        let mut items = vec![text_item(1, false), text_item(2, false), text_item(3, false)];
        let dup = ClipboardItem {
            id: 99,
            kind: ItemKind::Text("t3".into()), // 与最旧条目 id=3 同内容
            created_at: 99,
            pinned: false,
        };
        assert!(insert_dedup(&mut items, dup));
        assert_eq!(items.len(), 3, "promotion must not grow history");
        assert_eq!(items[0].id, 3, "old entry promoted to head");
        assert_eq!(items[0].created_at, 99, "recency: created_at refreshed to copy time");
        assert!(!items.iter().any(|it| it.id == 99), "no duplicate inserted");
    }

    /// 场景 3：无命中 → 正常头部新增
    #[test]
    fn dedup_no_match_inserts_at_head() {
        let mut items = vec![text_item(1, false)];
        assert!(insert_dedup(&mut items, text_item(2, false)));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, 2);
    }

    /// 加载路径上限统一：落盘可含 500 条（PERSIST_LIMIT）> 内存上限，
    /// enforce_limit 后 unpinned 收敛到 MAX_ITEMS、pinned 全保留
    #[test]
    fn loaded_history_capped_after_enforce() {
        let mut loaded: Vec<ClipboardItem> =
            (0..500u64).map(|i| text_item(i, i < 5)).collect(); // 前 5 条 pinned
        enforce_limit(&mut loaded);
        assert_eq!(loaded.iter().filter(|it| !it.pinned).count(), MAX_ITEMS);
        assert_eq!(loaded.iter().filter(|it| it.pinned).count(), 5);
    }
}
