//! 历史持久化：JSONL 单文件（app_data_dir/history.jsonl）
//! - 启动加载尾部 ≤500 条（仅文本；图片条目不落盘，防文件膨胀）
//! - 变更后防抖 2s 全量快照重写（tmp + rename 原子替换）

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::model::split_for_limit;
use crate::{AppHandle, ItemKind, Manager};

const PERSIST_LIMIT: usize = 500; // 最多落盘/加载的条数
const DEBOUNCE_MS: u64 = 2000; // 距最后一次变更多久后才落盘
const FLUSH_POLL_MS: u64 = 500; // flusher 轮询间隔

#[derive(Serialize, Deserialize)]
struct PersistRecord {
    id: u64,
    text: String,
    created_at: u64,
    #[serde(default)] // R2: 老文件无此字段视为 false（向后兼容）
    pinned: bool,
}

struct Inner {
    dirty: AtomicBool,
    last_change_ms: AtomicU64,
    /// R5: 序列化 request_save 与「flush 完成后清 dirty」两组操作——
    /// 消除无条件 store(false) 吞掉并发 request_save 的竞态窗口
    flush_lock: Mutex<()>,
}

#[derive(Clone)]
pub struct Storage {
    path: PathBuf,
    inner: std::sync::Arc<Inner>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Storage {
    pub fn new(dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&dir);
        Self {
            path: dir.join("history.jsonl"),
            inner: std::sync::Arc::new(Inner {
                dirty: AtomicBool::new(false),
                last_change_ms: AtomicU64::new(0),
                flush_lock: Mutex::new(()),
            }),
        }
    }

    /// 启动时加载持久化历史（坏行跳过；截断走 split_for_limit：
    /// pinned 全保留 + 非 pinned 保最新 PERSIST_LIMIT 条，与落盘口径对称）
    pub fn load(&self) -> Vec<crate::ClipboardItem> {
        let Ok(content) = fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        let mut items = Vec::new();
        for line in content.lines() {
            let Ok(rec) = serde_json::from_str::<PersistRecord>(line) else {
                continue; // 单行损坏不影响其余条目
            };
            items.push(crate::ClipboardItem {
                id: rec.id,
                kind: ItemKind::Text(rec.text),
                created_at: rec.created_at,
                pinned: rec.pinned,
            });
        }
        split_for_limit(&mut items, |it| it.pinned, PERSIST_LIMIT);
        items
    }

    /// 标记需要落盘（防抖由 flusher 线程处理）
    pub fn request_save(&self) {
        // 持锁写入：与 clear_dirty_if_unchanged 互斥，保证不会被清标志吞掉
        let _g = self.inner.flush_lock.lock().unwrap();
        self.inner.dirty.store(true, Ordering::SeqCst);
        self.inner.last_change_ms.store(now_ms(), Ordering::SeqCst);
    }

    /// R5: flush 完成后清 dirty——仅当本次 flush 对应的 last_change_ms 仍未变化。
    /// 与 request_save 互斥：并发到达的新变更会让时间戳推进 → 保持 dirty 由下轮 flush
    fn clear_dirty_if_unchanged(&self, changed_at: u64) {
        let _g = self.inner.flush_lock.lock().unwrap();
        if self.inner.last_change_ms.load(Ordering::SeqCst) == changed_at {
            self.inner.dirty.store(false, Ordering::SeqCst);
        }
    }

    /// R5: 立即同步落盘一次（app 退出路径 RunEvent::Exit 调用，消除 2s 防抖丢失窗口）
    pub fn flush_now(&self, app: &AppHandle) {
        if !self.inner.dirty.load(Ordering::SeqCst) {
            return;
        }
        let changed_at = self.inner.last_change_ms.load(Ordering::SeqCst);
        match self.snapshot_write(app) {
            Ok(n) => {
                self.clear_dirty_if_unchanged(changed_at);
                eprintln!("[clipmate] history flushed on exit: {n} records");
            }
            Err(e) => eprintln!("[clipmate] exit flush failed: {e}"),
        }
    }

    fn snapshot_write(&self, app: &AppHandle) -> std::io::Result<usize> {
        let state = app.state::<crate::AppState>();
        let items = state.items.lock().unwrap();
        // R8 修 Critic P2：pinned 全部保留 + 非 pinned 保最新 PERSIST_LIMIT 条。
        // 旧实现 take(PERSIST_LIMIT) 按位置截断，pinned>200 时尾部老 pinned 被
        // 静默丢出 jsonl，与 enforce_limit"pinned 永不淘汰"跨层矛盾
        let mut snapshot: Vec<&crate::ClipboardItem> = items.iter().collect();
        split_for_limit(&mut snapshot, |it| it.pinned, PERSIST_LIMIT);
        let mut buf = String::new();
        let mut n = 0;
        for it in snapshot {
            // 图片条目不持久化：png 体积会让 JSONL 急剧膨胀
            let ItemKind::Text(text) = &it.kind else { continue };
            let rec = PersistRecord { id: it.id, text: text.clone(), created_at: it.created_at, pinned: it.pinned };
            let line = serde_json::to_string(&rec).map_err(std::io::Error::other)?;
            buf.push_str(&line);
            buf.push('\n');
            n += 1;
        }
        drop(items);
        let tmp = self.path.with_extension("jsonl.tmp");
        fs::write(&tmp, &buf)?;
        fs::rename(&tmp, &self.path)?;
        Ok(n)
    }

    /// 后台防抖落盘线程：dirty 且距最后变更 ≥2s → 全量快照重写
    pub fn start_flusher(self, app: AppHandle) {
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(FLUSH_POLL_MS));
                if !self.inner.dirty.load(Ordering::SeqCst) {
                    continue;
                }
                let changed_at = self.inner.last_change_ms.load(Ordering::SeqCst);
                if now_ms().saturating_sub(changed_at) < DEBOUNCE_MS {
                    continue;
                }
                match self.snapshot_write(&app) {
                    Ok(n) => {
                        // 写期间若又有变更（last_change_ms 变化）则保持 dirty，下轮再写
                        self.clear_dirty_if_unchanged(changed_at);
                        eprintln!("[clipmate] history flushed: {n} records");
                    }
                    Err(e) => eprintln!("[clipmate] history flush failed: {e}"),
                }
            }
        });
    }
}

// ---------- tests (R5) ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_storage() -> Storage {
        let dir = std::env::temp_dir().join(format!("clipmate-test-{}", std::process::id()));
        Storage::new(dir)
    }

    /// Critic P2 场景：flush 期间新变更到达（last_change_ms 推进）→ dirty 必须保持，
    /// 否则该变更被吞、永不落盘
    #[test]
    fn clear_dirty_keeps_dirty_when_change_arrived_during_flush() {
        let s = tmp_storage();
        s.request_save();
        let changed_at = s.inner.last_change_ms.load(Ordering::SeqCst);
        // 模拟 snapshot_write 期间 request_save 到达：时间戳推进
        // （request_save 也可能落在同毫秒，故显式 store 更大值模拟严格推进）
        s.inner.last_change_ms.store(changed_at + 1000, Ordering::SeqCst);
        s.clear_dirty_if_unchanged(changed_at);
        assert!(
            s.inner.dirty.load(Ordering::SeqCst),
            "late request_save must not be swallowed"
        );
    }

    /// 无新变更时 flush 完成正常清 dirty
    #[test]
    fn clear_dirty_clears_when_no_new_change() {
        let s = tmp_storage();
        s.request_save();
        let changed_at = s.inner.last_change_ms.load(Ordering::SeqCst);
        s.clear_dirty_if_unchanged(changed_at);
        assert!(!s.inner.dirty.load(Ordering::SeqCst));
    }

    /// 并发压力：request_save 与 clear_dirty_if_unchanged 交错，
    /// 验证锁语义下两组操作可安全并发（不 panic、不死锁）
    #[test]
    fn concurrent_request_save_never_swallowed() {
        use std::sync::Arc;
        let s = Arc::new(tmp_storage());
        let stop = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let s = Arc::clone(&s);
            let stop = Arc::clone(&stop);
            handles.push(std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    s.request_save();
                }
            }));
        }
        for _ in 0..200 {
            let changed_at = s.inner.last_change_ms.load(Ordering::SeqCst);
            s.clear_dirty_if_unchanged(changed_at);
        }
        stop.store(true, Ordering::SeqCst);
        for h in handles {
            h.join().unwrap();
        }
        // 最后一次 request_save 的 dirty 不应被吞：writer 停止后状态收敛
        s.request_save();
        let changed_at = s.inner.last_change_ms.load(Ordering::SeqCst);
        s.clear_dirty_if_unchanged(changed_at);
        assert!(!s.inner.dirty.load(Ordering::SeqCst));
    }

    /// R8 修 Critic P2 端到端：jsonl 总行数 > PERSIST_LIMIT 且 pinned 在文件尾部
    /// （最老位置）——旧按位置截断/drain 头部都会丢 pinned；新 load 全保留。
    /// 场景 = 500 unpinned + 300 pinned（pinned>200 时旧 snapshot_write 的 take
    /// 截断会静默丢尾部 pinned）
    #[test]
    fn load_keeps_all_pinned_when_file_exceeds_limit() {
        let s = tmp_storage();
        let mut buf = String::new();
        // jsonl 头=最新：500 条 unpinned 在前，300 条 pinned 沉在尾部
        for i in 0..500u64 {
            let rec = PersistRecord { id: i, text: format!("t{i}"), created_at: i, pinned: false };
            buf.push_str(&serde_json::to_string(&rec).unwrap());
            buf.push('\n');
        }
        for i in 1000..1300u64 {
            let rec = PersistRecord { id: i, text: format!("p{i}"), created_at: i, pinned: true };
            buf.push_str(&serde_json::to_string(&rec).unwrap());
            buf.push('\n');
        }
        fs::write(&s.path, buf).unwrap();
        let items = s.load();
        assert_eq!(
            items.iter().filter(|it| it.pinned).count(),
            300,
            "pinned entries must all survive load, none silently dropped"
        );
        assert_eq!(items.iter().filter(|it| !it.pinned).count(), 500);
        // 非 pinned 保最新（jsonl 头部）：unpinned id 0..499 全在
        assert!(items.iter().filter(|it| !it.pinned).all(|it| it.id < 500));
    }
}
