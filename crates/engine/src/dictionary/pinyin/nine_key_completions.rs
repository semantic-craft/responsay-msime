//! 九键精确模式下，已命中长读法后仍保留可继续输入的尾段补全。

use super::*;

const GRID_TAIL_COMPLETION_LIMIT: usize = 12;

impl PinyinDatabase {
    pub(crate) fn query_grid_tail_completions(
        &self,
        segments: &[String],
        key: &str,
    ) -> Vec<DictRow> {
        if segments.len() < 4 || !has_only_complete_pinyin_segments(segments) {
            return Vec::new();
        }
        let Some(table) = build_table_name(segments) else {
            return Vec::new();
        };
        let upper = key_prefix_upper_bound(key);
        if self.overlay.has_pinyin() {
            let guard = self.overlay.read();
            if let Some(table_overlay) = guard.pinyin.get(&table) {
                let fetch_limit =
                    GRID_TAIL_COMPLETION_LIMIT.saturating_add(table_overlay.deleted.len());
                let bound = sql_limit(fetch_limit);
                let sql = format!(
                    "SELECT \"key\", \"value\", \"weight\" FROM \"{table}\" WHERE \"key\" > ?1 AND \"key\" < ?2 ORDER BY \"weight\" DESC LIMIT {bound}"
                );
                let base = self.rows(
                    &sql,
                    [key, upper.as_str()],
                    query_capacity(GRID_TAIL_COMPLETION_LIMIT),
                );
                return merge_pinyin_overlay_rows(
                    base,
                    table_overlay,
                    GRID_TAIL_COMPLETION_LIMIT,
                    |row| row.key.as_str() > key && row.key.as_str() < upper.as_str(),
                );
            }
        }
        let bound = sql_limit(GRID_TAIL_COMPLETION_LIMIT);
        let sql = format!(
            "SELECT \"key\", \"value\", \"weight\" FROM \"{table}\" WHERE \"key\" > ?1 AND \"key\" < ?2 ORDER BY \"weight\" DESC LIMIT {bound}"
        );
        self.rows(
            &sql,
            [key, upper.as_str()],
            query_capacity(GRID_TAIL_COMPLETION_LIMIT),
        )
    }
}
