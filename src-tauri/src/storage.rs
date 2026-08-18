//! 历史持久化：JSONL 单文件（app_data_dir/history.jsonl）
//! - 启动加载尾部 ≤500 条（仅文本；图片条目不落盘，防文件膨胀）
//! - 变更后防抖 2s 全量快照重写（tmp + rename 原子替换）

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

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
            }),
        }
    }

    /// 启动时加载持久化历史（尾部 PERSIST_LIMIT 条，坏行跳过）
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
        let len = items.len();
        if len > PERSIST_LIMIT {
            items.drain(..len - PERSIST_LIMIT);
        }
        items
    }

    /// 标记需要落盘（防抖由 flusher 线程处理）
    pub fn request_save(&self) {
        self.inner.dirty.store(true, Ordering::SeqCst);
        self.inner.last_change_ms.store(now_ms(), Ordering::SeqCst);
    }

    fn snapshot_write(&self, app: &AppHandle) -> std::io::Result<usize> {
        let state = app.state::<crate::AppState>();
        let items = state.items.lock().unwrap();
        let mut buf = String::new();
        let mut n = 0;
        for it in items.iter().take(PERSIST_LIMIT) {
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
                        if self.inner.last_change_ms.load(Ordering::SeqCst) == changed_at {
                            self.inner.dirty.store(false, Ordering::SeqCst);
                        }
                        eprintln!("[clipmate] history flushed: {n} records");
                    }
                    Err(e) => eprintln!("[clipmate] history flush failed: {e}"),
                }
            }
        });
    }
}
