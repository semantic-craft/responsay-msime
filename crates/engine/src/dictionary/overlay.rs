//! In-memory user dictionary overlay (`pinyin`, `quick`, `english`) for read-only lexicon sessions.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;

use super::pinyin::INSERTED_WEIGHT;
use super::DictRow;
use crate::error::{EngineError, Result};
use crate::format::quanpin_table;

const MAX_OVERLAY_KEY_BYTES: usize = 512;
const MAX_OVERLAY_VALUE_BYTES: usize = 4 * 1024;
const MAX_OVERLAY_DISPLAY_BYTES: usize = 4 * 1024;

fn default_overlay_weight() -> i64 {
    INSERTED_WEIGHT
}

#[derive(Deserialize)]
struct UserOverlayWire {
    version: u32,
    #[serde(default)]
    revision: String,
    #[serde(default)]
    entries: Vec<UserOverlayWireEntry>,
}

#[derive(Deserialize)]
struct UserOverlayWireEntry {
    dictionary: String,
    key: String,
    value: String,
    operation: String,
    #[serde(default = "default_overlay_weight")]
    weight: i64,
    #[serde(default)]
    display: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinyinOverlayRow {
    pub key: String,
    pub jp: String,
    pub value: String,
    pub weight: i64,
}

#[derive(Debug, Clone, Default)]
pub struct PinyinTableOverlay {
    pub upserts: Vec<PinyinOverlayRow>,
    pub deleted: HashSet<(String, String)>,
}

impl PinyinTableOverlay {
    pub fn is_empty(&self) -> bool {
        self.upserts.is_empty() && self.deleted.is_empty()
    }

    pub fn is_deleted(&self, key: &str, value: &str) -> bool {
        !self.deleted.is_empty() && self.deleted.iter().any(|(dk, dv)| dk == key && dv == value)
    }

    pub fn find_upsert_weight(&self, key: &str, value: &str) -> Option<i64> {
        self.upserts
            .iter()
            .find(|row| row.key == key && row.value == value)
            .map(|row| row.weight)
    }

    pub fn has_upsert(&self, key: &str, value: &str) -> bool {
        self.find_upsert_weight(key, value).is_some()
    }

    fn apply_upsert(&mut self, key: String, jp: String, value: String, weight: i64) {
        self.deleted
            .retain(|(dk, dv)| !(dk == &key && dv == &value));
        if let Some(existing) = self
            .upserts
            .iter_mut()
            .find(|row| row.key == key && row.value == value)
        {
            existing.jp = jp;
            existing.weight = weight;
        } else {
            self.upserts.push(PinyinOverlayRow {
                key,
                jp,
                value,
                weight,
            });
        }
    }

    fn apply_delete(&mut self, key: String, value: String) {
        self.upserts
            .retain(|row| !(row.key == key && row.value == value));
        self.deleted.insert((key, value));
    }

    fn finalize(&mut self) {
        self.upserts.sort_by_key(|row| Reverse(row.weight));
    }
}

#[derive(Debug, Clone, Default)]
pub struct QuickOverlay {
    pub upserts: Vec<DictRow>,
    pub deleted: HashSet<(String, String)>,
}

impl QuickOverlay {
    pub fn is_empty(&self) -> bool {
        self.upserts.is_empty() && self.deleted.is_empty()
    }

    pub fn is_deleted(&self, key: &str, value: &str) -> bool {
        !self.deleted.is_empty() && self.deleted.iter().any(|(dk, dv)| dk == key && dv == value)
    }

    pub fn has_upsert(&self, key: &str, value: &str) -> bool {
        self.upserts
            .iter()
            .any(|row| row.key == key && row.value == value)
    }

    fn apply_upsert(&mut self, key: String, value: String, weight: i64) {
        self.deleted
            .retain(|(dk, dv)| !(dk == &key && dv == &value));
        if let Some(existing) = self
            .upserts
            .iter_mut()
            .find(|row| row.key == key && row.value == value)
        {
            existing.weight = weight;
        } else {
            self.upserts.push(DictRow { key, value, weight });
        }
    }

    fn apply_delete(&mut self, key: String, value: String) {
        self.upserts
            .retain(|row| !(row.key == key && row.value == value));
        self.deleted.insert((key, value));
    }

    fn finalize(&mut self) {
        self.upserts.sort_by(|left, right| {
            right
                .weight
                .cmp(&left.weight)
                .then_with(|| left.key.cmp(&right.key))
                .then_with(|| left.value.cmp(&right.value))
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnglishOverlayRow {
    pub word: String,
    pub display: String,
    pub weight: i64,
}

#[derive(Debug, Clone, Default)]
pub struct EnglishOverlay {
    pub upserts: Vec<EnglishOverlayRow>,
    pub deleted: HashSet<(String, String)>,
}

impl EnglishOverlay {
    pub fn is_empty(&self) -> bool {
        self.upserts.is_empty() && self.deleted.is_empty()
    }

    pub fn is_deleted(&self, word: &str, display: &str) -> bool {
        !self.deleted.is_empty()
            && self
                .deleted
                .iter()
                .any(|(dw, dd)| dw == word && dd == display)
    }

    pub fn has_upsert(&self, word: &str, display: &str) -> bool {
        self.upserts
            .iter()
            .any(|row| row.word == word && row.display == display)
    }

    fn apply_upsert(&mut self, word: String, display: String, weight: i64) {
        self.deleted
            .retain(|(dw, dd)| !(dw == &word && dd == &display));
        if let Some(existing) = self
            .upserts
            .iter_mut()
            .find(|row| row.word == word && row.display == display)
        {
            existing.weight = weight;
        } else {
            self.upserts.push(EnglishOverlayRow {
                word,
                display,
                weight,
            });
        }
    }

    fn apply_delete(&mut self, word: String, display: String) {
        self.upserts
            .retain(|row| !(row.word == word && row.display == display));
        self.deleted.insert((word, display));
    }

    fn finalize(&mut self) {
        self.upserts.sort_by(|left, right| {
            right
                .weight
                .cmp(&left.weight)
                .then_with(|| left.word.len().cmp(&right.word.len()))
                .then_with(|| left.word.cmp(&right.word))
                .then_with(|| left.display.cmp(&right.display))
        });
    }
}

#[derive(Debug, Clone, Default)]
pub struct UserOverlayData {
    pub revision: String,
    pub pinyin: HashMap<String, PinyinTableOverlay>,
    pub quick: QuickOverlay,
    pub english: EnglishOverlay,
}

impl UserOverlayData {
    pub fn is_empty(&self) -> bool {
        self.pinyin.values().all(PinyinTableOverlay::is_empty)
            && self.quick.is_empty()
            && self.english.is_empty()
    }

    pub fn has_pinyin(&self) -> bool {
        self.pinyin.values().any(|table| !table.is_empty())
    }

    pub fn has_quick(&self) -> bool {
        !self.quick.is_empty()
    }

    pub fn has_english(&self) -> bool {
        !self.english.is_empty()
    }

    pub fn from_json(json: &str) -> Result<Self> {
        let wire: UserOverlayWire = serde_json::from_str(json)
            .map_err(|error| EngineError::invalid(format!("invalid user overlay json: {error}")))?;
        if wire.version != 1 {
            return Err(EngineError::invalid(format!(
                "unsupported user overlay version {}",
                wire.version
            )));
        }
        let mut data = Self {
            revision: wire.revision,
            ..Self::default()
        };
        for entry in wire.entries {
            if entry.key.is_empty() || entry.key.len() > MAX_OVERLAY_KEY_BYTES {
                return Err(EngineError::invalid("invalid user overlay key"));
            }
            if entry.value.is_empty() || entry.value.len() > MAX_OVERLAY_VALUE_BYTES {
                return Err(EngineError::invalid("invalid user overlay value"));
            }
            if entry.display.len() > MAX_OVERLAY_DISPLAY_BYTES {
                return Err(EngineError::invalid("invalid user overlay display"));
            }
            let is_delete = match entry.operation.as_str() {
                "upsert" => false,
                "delete" => true,
                other => {
                    return Err(EngineError::invalid(format!(
                        "unknown user overlay operation {other}"
                    )))
                }
            };
            match entry.dictionary.as_str() {
                "pinyin" => {
                    let Some((table, jp)) = pinyin_table_and_jp(&entry.key) else {
                        return Err(EngineError::invalid("invalid pinyin overlay key"));
                    };
                    let slot = data.pinyin.entry(table).or_default();
                    if is_delete {
                        slot.apply_delete(entry.key, entry.value);
                    } else if entry.weight >= 1 {
                        slot.apply_upsert(entry.key, jp, entry.value, entry.weight);
                    }
                }
                "quick" => {
                    if is_delete {
                        data.quick.apply_delete(entry.key, entry.value);
                    } else {
                        data.quick
                            .apply_upsert(entry.key, entry.value, entry.weight);
                    }
                }
                "english" => {
                    let display = if entry.display.is_empty() {
                        entry.value
                    } else {
                        entry.display
                    };
                    if is_delete {
                        data.english.apply_delete(entry.key, display);
                    } else {
                        data.english.apply_upsert(entry.key, display, entry.weight);
                    }
                }
                other => {
                    return Err(EngineError::invalid(format!(
                        "unknown user overlay dictionary {other}"
                    )))
                }
            }
        }
        data.finalize();
        Ok(data)
    }

    /// Performs a one-shot `SQLITE_OPEN_READ_ONLY` (`busy_timeout = 0`) replay of
    /// `user_dictionary_operations` from `journal_path`, dropping the SQLite connection
    /// immediately before returning so zero descriptors or locks remain open on `msime_user.db`.
    pub fn load_one_shot_journal(journal_path: &Path) -> Option<Self> {
        if journal_path.as_os_str().is_empty() {
            return None;
        }
        let metadata = std::fs::symlink_metadata(journal_path).ok()?;
        if !metadata.file_type().is_file() {
            return None;
        }
        let path = crate::paths::sqlite_read_only_path_no_follow(journal_path).ok()?;
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NOFOLLOW
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()?;
        if connection.busy_timeout(Duration::ZERO).is_err() {
            drop(connection);
            return None;
        }
        let mut data = Self::default();
        {
            let Ok(mut statement) = connection.prepare(
                "SELECT dictionary, key, value, operation, weight, display \
                 FROM user_dictionary_operations \
                 WHERE dictionary IN ('pinyin', 'quick', 'english') \
                 ORDER BY updated_at, rowid",
            ) else {
                return None;
            };
            let Ok(mut rows) = statement.query([]) else {
                return None;
            };
            while let Ok(Some(row)) = rows.next() {
                let (Ok(kind), Ok(key), Ok(value), Ok(op)) = (
                    row.get_ref(0)
                        .ok()
                        .and_then(|v| v.as_bytes().ok())
                        .ok_or(()),
                    row.get_ref(1)
                        .ok()
                        .and_then(|v| v.as_bytes().ok())
                        .ok_or(()),
                    row.get_ref(2)
                        .ok()
                        .and_then(|v| v.as_bytes().ok())
                        .ok_or(()),
                    row.get_ref(3)
                        .ok()
                        .and_then(|v| v.as_bytes().ok())
                        .ok_or(()),
                ) else {
                    continue;
                };
                if key.is_empty()
                    || key.len() > MAX_OVERLAY_KEY_BYTES
                    || value.is_empty()
                    || value.len() > MAX_OVERLAY_VALUE_BYTES
                {
                    continue;
                }
                let weight = row.get::<_, i64>(4).unwrap_or(0);
                let display_bytes = row
                    .get_ref(5)
                    .ok()
                    .and_then(|v| v.as_bytes().ok())
                    .unwrap_or(&[]);
                if display_bytes.len() > MAX_OVERLAY_DISPLAY_BYTES {
                    continue;
                }
                let is_delete = match op {
                    b"upsert" => false,
                    b"delete" => true,
                    _ => continue,
                };
                let (Ok(key_str), Ok(value_str), Ok(display_str)) = (
                    std::str::from_utf8(key),
                    std::str::from_utf8(value),
                    std::str::from_utf8(display_bytes),
                ) else {
                    continue;
                };
                match kind {
                    b"pinyin" => {
                        if !is_delete && weight < 1 {
                            continue;
                        }
                        let Some((table, jp)) = pinyin_table_and_jp(key_str) else {
                            continue;
                        };
                        let slot = data.pinyin.entry(table).or_default();
                        if is_delete {
                            slot.apply_delete(key_str.to_owned(), value_str.to_owned());
                        } else {
                            slot.apply_upsert(key_str.to_owned(), jp, value_str.to_owned(), weight);
                        }
                    }
                    b"quick" => {
                        if is_delete {
                            data.quick
                                .apply_delete(key_str.to_owned(), value_str.to_owned());
                        } else {
                            data.quick.apply_upsert(
                                key_str.to_owned(),
                                value_str.to_owned(),
                                weight,
                            );
                        }
                    }
                    b"english" => {
                        let effective_display = if display_str.is_empty() {
                            value_str
                        } else {
                            display_str
                        };
                        if is_delete {
                            data.english
                                .apply_delete(key_str.to_owned(), effective_display.to_owned());
                        } else {
                            data.english.apply_upsert(
                                key_str.to_owned(),
                                effective_display.to_owned(),
                                weight,
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        drop(connection);
        data.finalize();
        (!data.is_empty()).then_some(data)
    }

    fn finalize(&mut self) {
        self.pinyin.retain(|_, table| {
            table.finalize();
            !table.is_empty()
        });
        self.quick.finalize();
        self.english.finalize();
    }
}

pub fn pinyin_table_and_jp(key: &str) -> Option<(String, String)> {
    let segments: Vec<&str> = key.split('\'').collect();
    if segments.iter().any(|segment| segment.is_empty()) {
        return None;
    }
    let table = quanpin_table(segments.len(), segments[0].as_bytes()[0])?;
    let jp: String = segments
        .iter()
        .filter_map(|segment| segment.chars().next())
        .collect();
    Some((table, jp))
}

#[derive(Default)]
struct UserOverlayInner {
    generation: AtomicU64,
    has_pinyin: AtomicBool,
    has_quick: AtomicBool,
    has_english: AtomicBool,
    data: RwLock<UserOverlayData>,
}

#[derive(Clone, Default)]
pub struct SharedUserOverlay {
    inner: Arc<UserOverlayInner>,
}

impl SharedUserOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }

    pub fn has_pinyin(&self) -> bool {
        self.inner.has_pinyin.load(Ordering::Acquire)
    }

    pub fn has_quick(&self) -> bool {
        self.inner.has_quick.load(Ordering::Acquire)
    }

    pub fn has_english(&self) -> bool {
        self.inner.has_english.load(Ordering::Acquire)
    }

    pub fn has_english_word(&self, word: &str) -> bool {
        !word.is_empty()
            && self.has_english()
            && self
                .read()
                .english
                .upserts
                .iter()
                .any(|row| row.word == word)
    }

    pub fn read(&self) -> RwLockReadGuard<'_, UserOverlayData> {
        self.inner
            .data
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn replace(&self, next: UserOverlayData) {
        let has_pinyin = next.has_pinyin();
        let has_quick = next.has_quick();
        let has_english = next.has_english();
        {
            let mut guard = self
                .inner
                .data
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = next;
        }
        self.inner.has_pinyin.store(has_pinyin, Ordering::Release);
        self.inner.has_quick.store(has_quick, Ordering::Release);
        self.inner.has_english.store(has_english, Ordering::Release);
        self.inner.generation.fetch_add(1, Ordering::AcqRel);
    }
}
