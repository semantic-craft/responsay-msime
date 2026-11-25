//! Resetting learned data (user-dictionary.md §13, bridge.cpp:588-691): fresh copies of the packaged dictionaries and an empty journal, swapped in with backups and rolled back on any failure. Sibling names are built by concatenation so non-ASCII profile directories work.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::OpenFlags;

use crate::assets;
use crate::diagnostics;
use crate::error::{EngineError, Result};
use crate::paths::RuntimePaths;
use crate::types::SchemeSet;
use crate::user_dictionary::generation::{
    copy_database, copy_private_file, merge_split_wubi, weakly_canonical,
};
use crate::user_dictionary::journal::{
    checkpoint_journal, close_cached_journals, ensure_schema, open_database,
};
use crate::user_dictionary::replay::{replay, replay_english};

/// SQLite files that sit beside the journal. A leftover `-wal` would bring the learned data the user just erased back on the next open.
const JOURNAL_SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

const CLEAR_AUTO_LEARNED_SQL: &str = "\
DELETE FROM user_dictionary_operations WHERE user_inserted = 0;\
DELETE FROM candidate_selection_state;\
DELETE FROM fixed_candidate_positions;\
DELETE FROM pick_transitions;\
DELETE FROM pinyin_typo_counts;\
DELETE FROM pinyin_autocorrect_suppressions;\
DELETE FROM shuangpin_typo_counts;\
DELETE FROM shuangpin_autocorrect_suppressions;\
DELETE FROM pinned_candidates;\
DELETE FROM personal_bigram;\
DELETE FROM personal_trigram;\
DELETE FROM quick_phrase_slots;";

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResetMode {
    Full,
    AutoLearnedOnly,
}

/// One file being swapped: the fresh copy is published over `target` while the original waits at `backup` until every file is in place.
struct Replacement {
    target: PathBuf,
    temporary: PathBuf,
    backup: PathBuf,
    had_original: bool,
    published: bool,
}

/// The caller must have quiesced every session on these paths.
///
/// 自己取独占的维护租约（`DictionaryAccess::try_maintenance`），在建好两个目录之后、动任何文件之前：还有会话或别的维护持着租约时以 `Busy`（`DICTIONARY_MAINTENANCE_BUSY`）失败，什么也不换。
pub fn reset_learned_data(paths: &RuntimePaths) -> Result<()> {
    reset(paths, true, true, true, ResetMode::Full)
}

/// [`reset_learned_data`]，按代次准备时的方案集合（[`crate::prepare_runtime_paths_for`] 的 `schemes`）换回词库：只换回、只要求这个集合读的那几个，与准备代次的规则相同。不读 `msime-pinyin.db` 的集合不要求也不复制它；不要求 `msime-english.db` 的集合在资源目录带了它时换回它，没带时只清空日志。还有会话开着时以 `Busy` 失败，规则同 [`reset_learned_data`]。
pub fn reset_learned_data_for(paths: &RuntimePaths, schemes: SchemeSet) -> Result<()> {
    let main_dictionary = schemes.reads_main_dictionary();
    reset(
        paths,
        main_dictionary,
        main_dictionary || schemes.requires_english_dictionary(),
        true,
        ResetMode::Full,
    )
}

/// Clears only auto-learned state (`user_inserted = 0` operations, personal n-gram tables, typo profiles, pick transitions, pinned/fixed positions, selection counts, and `responsay_hotwords` rows with `manual = 0`), restores packaged dictionaries with `user_inserted = 1` manual operations replayed, and invalidates in-memory personal models while leaving `user_inserted = 1` and `responsay_hotwords` (`manual = 1`) manual entries untouched.
pub fn reset_auto_learned_data(paths: &RuntimePaths) -> Result<()> {
    reset(paths, true, true, true, ResetMode::AutoLearnedOnly)
}

/// [`reset_auto_learned_data`] scoped to the dictionaries prepared for `schemes`.
pub fn reset_auto_learned_data_for(paths: &RuntimePaths, schemes: SchemeSet) -> Result<()> {
    let main_dictionary = schemes.reads_main_dictionary();
    reset(
        paths,
        main_dictionary,
        main_dictionary || schemes.requires_english_dictionary(),
        true,
        ResetMode::AutoLearnedOnly,
    )
}

/// Scrubs only `msime_user.db` under `user_data` (preserving `user_inserted = 1` and `responsay_hotwords` with `manual = 1`) and invalidates in-memory personal models, acquiring an exclusive maintenance lease on `user_data`.
pub fn reset_auto_learned_journal(user_data: &Path) -> Result<()> {
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    let absolute = user_data.has_root();
    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    let absolute = user_data.is_absolute();
    if !absolute {
        return Err(EngineError::invalid(
            diagnostics::RUNTIME_DIRECTORIES_MUST_BE_ABSOLUTE,
        ));
    }
    reject_redirected_directory(user_data)?;
    fs::create_dir_all(user_data)?;
    reject_redirected_directory(user_data)?;
    let _access = crate::DictionaryAccess::try_maintenance(user_data, user_data)?
        .ok_or_else(|| EngineError::busy(diagnostics::DICTIONARY_MAINTENANCE_BUSY))?;
    scrub_journal_in_place(&user_data.join(assets::USER_JOURNAL))
}

/// [`reset_learned_data`]，`main_dictionary` 为假（代次里没有 `msime-pinyin.db`，见 `SchemeSet::reads_main_dictionary`）时不要求也不复制 `msime-pinyin.db`，`msime-english.db` 在资源目录里有才换回，然后清空日志。不取维护租约，调用方（host 门面背后的 host-api）已经持有。
pub fn reset_learned_data_with(paths: &RuntimePaths, main_dictionary: bool) -> Result<()> {
    reset(paths, main_dictionary, main_dictionary, false, ResetMode::Full)
}

/// `english_required` 为假时 `msime-english.db` 在资源目录里有才换回。`take_lease` 为真时自己取维护租约，持到换完为止。
fn reset(
    paths: &RuntimePaths,
    main_dictionary: bool,
    english_required: bool,
    take_lease: bool,
    mode: ResetMode,
) -> Result<()> {
    paths.validate()?;
    if weakly_canonical(&paths.resources)? == weakly_canonical(&paths.dictionaries)? {
        return Err(EngineError::invalid(diagnostics::RESET_IN_PLACE));
    }
    // 不要求的英文库看资源目录里有没有这一项，符号链接也算有，交给下面的检查按原来的方式拒绝。
    let english = english_required
        || fs::symlink_metadata(paths.resource(assets::ENGLISH_DICTIONARY)).is_ok();
    let names: Vec<&str> = [
        (main_dictionary, assets::MAIN_DICTIONARY),
        (english, assets::ENGLISH_DICTIONARY),
    ]
    .into_iter()
    .filter_map(|(present, name)| present.then_some(name))
    .collect();
    let sources: Vec<PathBuf> = names.iter().map(|name| paths.resource(name)).collect();
    if !sources.iter().all(|source| is_real_file(source)) {
        return Err(EngineError::failed(
            diagnostics::PACKAGED_DICTIONARY_UNAVAILABLE,
        ));
    }
    reject_redirected_directory(&paths.user_data)?;
    reject_redirected_directory(&paths.dictionaries)?;
    fs::create_dir_all(&paths.user_data)?;
    fs::create_dir_all(&paths.dictionaries)?;
    reject_redirected_directory(&paths.user_data)?;
    reject_redirected_directory(&paths.dictionaries)?;
    let _access = if take_lease {
        Some(crate::access::maintenance(paths)?)
    } else {
        None
    };
    // Every cached handle on the files about to be replaced goes first; on Windows an open handle blocks the rename.
    close_cached_journals();

    // Unique per call, so a reset interrupted earlier never collides with this one's files. A clock before 1970 would only make the names less unique, not wrong.
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos())
        .to_string();
    let journal = paths.user(assets::USER_JOURNAL);
    // 重置会替换整份日志；时间下限属于本机同步屏障，不能随学习行清掉。
    let reset_floor = super::learning_floor::read_path(&journal)?;
    // 日志是 WAL 时，最近的学习可能还在 `-wal` 里、没并回主文件。先并回并清空 `-wal`：失败回滚或被杀后放回的备份本身就是完整的日志，新日志发布后、旁路文件删掉之前，原名旁边也只剩一个空 `-wal`，套不到新日志上。
    checkpoint_journal(&journal);
    // 词库在前、日志在最后：`swap` 按顺序把 `sources` 配给前面的词库。
    let mut replacements: Vec<Replacement> = names
        .iter()
        .map(|name| paths.dictionary(name))
        .chain([journal.clone()])
        .map(|target| Replacement {
            temporary: affixed(&target, ".reset.", &stamp),
            backup: affixed(&target, ".backup.", &stamp),
            target,
            had_original: false,
            published: false,
        })
        .collect();
    let journal_temporary = replacements[names.len()].temporary.clone();

    let prepared = stage_journal(&journal, &journal_temporary, reset_floor, mode);
    if prepared.is_err() {
        remove_ignoring_errors(&journal_temporary);
        return Err(EngineError::failed(diagnostics::RESET_JOURNAL_FAILED));
    }

    let swapped = swap(
        &mut replacements,
        &paths.resources,
        main_dictionary,
        english,
        &sources,
        &journal,
        &journal_temporary,
        mode,
    );
    match swapped {
        Ok(()) => {
            // Every replacement is published; the originals are no longer needed.
            for replacement in &replacements {
                remove_ignoring_errors(&replacement.backup);
            }
            super::ngram_store::clear_journal(&journal);
            super::typo_profile::PersonalTypoProfile::invalidate_journal(&journal);
            close_cached_journals();
            Ok(())
        }
        Err(error) => {
            restore(&replacements);
            Err(error)
        }
    }
}

fn scrub_journal_in_place(journal: &Path) -> Result<()> {
    close_cached_journals();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos())
        .to_string();
    let reset_floor = super::learning_floor::read_path(journal)?;
    checkpoint_journal(journal);
    let mut replacements = [Replacement {
        temporary: affixed(journal, ".reset.", &stamp),
        backup: affixed(journal, ".backup.", &stamp),
        target: journal.to_path_buf(),
        had_original: false,
        published: false,
    }];
    let journal_temporary = replacements[0].temporary.clone();
    if stage_journal(
        journal,
        &journal_temporary,
        reset_floor,
        ResetMode::AutoLearnedOnly,
    )
    .is_err()
    {
        remove_ignoring_errors(&journal_temporary);
        return Err(EngineError::failed(diagnostics::RESET_JOURNAL_FAILED));
    }
    match swap(
        &mut replacements,
        Path::new(""),
        false,
        false,
        &[],
        journal,
        &journal_temporary,
        ResetMode::AutoLearnedOnly,
    ) {
        Ok(()) => {
            remove_ignoring_errors(&replacements[0].backup);
            super::ngram_store::clear_journal(journal);
            super::typo_profile::PersonalTypoProfile::invalidate_journal(journal);
            close_cached_journals();
            Ok(())
        }
        Err(error) => {
            restore(&replacements);
            Err(error)
        }
    }
}

fn stage_journal(
    journal: &Path,
    journal_temporary: &Path,
    reset_floor: u64,
    mode: ResetMode,
) -> Result<()> {
    if mode == ResetMode::AutoLearnedOnly && is_real_file(journal) {
        copy_database(journal, journal_temporary)?;
        let connection = open_database(journal_temporary, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        ensure_schema(&connection)?;
        connection.execute_batch("BEGIN IMMEDIATE")?;
        let scrubbed = (|| -> Result<()> {
            super::learning_floor::advance_in(&connection, "main", reset_floor)?;
            connection.execute_batch(CLEAR_AUTO_LEARNED_SQL)?;
            let has_hotwords_manual: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('responsay_hotwords') WHERE name='manual')",
                [],
                |row| row.get(0),
            )?;
            if has_hotwords_manual {
                connection.execute_batch("DELETE FROM responsay_hotwords WHERE manual = 0;")?;
            }
            Ok(())
        })();
        match scrubbed {
            Ok(()) => {
                connection.execute_batch("COMMIT")?;
                let _ = connection.execute_batch("VACUUM");
                return Ok(());
            }
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error);
            }
        }
    }

    let connection = open_database(
        journal_temporary,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    ensure_schema(&connection)?;
    super::learning_floor::advance_in(&connection, "main", reset_floor)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn swap(
    replacements: &mut [Replacement],
    resources: &Path,
    main_dictionary: bool,
    english: bool,
    sources: &[PathBuf],
    journal: &Path,
    journal_temporary: &Path,
    mode: ResetMode,
) -> Result<()> {
    // A plain copy, as the reference made: the packaged bundle is immutable, so it has no WAL to lose.
    for (source, replacement) in sources.iter().zip(replacements.iter()) {
        copy_private_file(source, &replacement.temporary)
            .map_err(|_| EngineError::failed(diagnostics::RESET_STAGE_DICTIONARIES_FAILED))?;
    }
    // 与准备代次相同：单独发布的五笔码表并回新的工作主词库，否则重置之后五笔学习与删词都找不到表。没有工作主词库时 `replacements[0]` 是英文词库或日志，不并。
    if main_dictionary {
        merge_split_wubi(resources, &replacements[0].temporary)
            .map_err(|_| EngineError::failed(diagnostics::RESET_STAGE_DICTIONARIES_FAILED))?;
    }
    if mode == ResetMode::AutoLearnedOnly {
        let replayed = match (main_dictionary, english) {
            (true, true) => replay(
                journal_temporary,
                &replacements[0].temporary,
                &replacements[1].temporary,
            ),
            (false, true) => replay_english(journal_temporary, &replacements[0].temporary),
            _ => crate::ReplayResult::default(),
        };
        if replayed.failed != 0 || !replayed.error.is_empty() {
            return Err(EngineError::failed(diagnostics::RESET_JOURNAL_FAILED));
        }
    }
    for replacement in replacements.iter_mut() {
        replacement.had_original = replacement.target.exists();
        if replacement.had_original {
            if replacement.backup.exists() {
                remove_ignoring_errors(&replacement.backup);
            }
            // The original is held aside until every replacement is published.
            fs::rename(&replacement.target, &replacement.backup)
                .map_err(|_| EngineError::failed(diagnostics::RESET_STAGE_FAILED))?;
        }
        fs::rename(&replacement.temporary, &replacement.target)
            .map_err(|_| EngineError::failed(diagnostics::RESET_PUBLISH_FAILED))?;
        replacement.published = true;
    }
    for suffix in JOURNAL_SIDECARS {
        let mut sidecar = journal.as_os_str().to_owned();
        sidecar.push(suffix);
        match fs::remove_file(PathBuf::from(sidecar)) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Undo in reverse order: drop what was published, put each original back, remove the fresh copies. Best effort, since the error being returned is the one that matters (bridge.cpp:648-655).
fn restore(replacements: &[Replacement]) {
    for replacement in replacements.iter().rev() {
        if replacement.published {
            remove_ignoring_errors(&replacement.target);
        }
        if replacement.had_original && replacement.backup.exists() {
            let _ = fs::rename(&replacement.backup, &replacement.target);
        }
        remove_ignoring_errors(&replacement.temporary);
    }
}

/// 重置在两次改名之间被杀时，从时间戳最新的普通文件备份恢复缺失的 `target`，再删除源文件名下同一时间戳没发布的 `.reset.` 副本。词库现名没有备份时也查找合法旧名备份，发布到请求的现名，供代次回放使用；现有文件与备份都按现名优先。准备代次时在打开词库和日志之前调用。
///
/// `target` 在（包括符号链接）时不动它，最终发布也不得覆盖检查之后竞争者放回的文件；失败保留备份。恢复后原重置进程若还活着，它缺少已删除的 `.reset.` 副本，后续发布失败会走原有回滚；这里不提供跨文件事务原子性。
pub(crate) fn restore_interrupted_reset(target: &Path) {
    match fs::symlink_metadata(target) {
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        _ => return,
    }
    // 旧文件挪开后 `RuntimePaths::dictionary` 已看不到它，只会给出现名；备份仍使用旧文件名。
    let backup = newest_reset_backup(target).or_else(|| {
        let name = target.file_name()?.to_str()?;
        let (_, legacy) = assets::LEGACY_NAMES
            .iter()
            .find(|(current, _)| *current == name)?;
        let legacy = target.with_file_name(legacy);
        match fs::symlink_metadata(&legacy) {
            Err(error) if error.kind() == ErrorKind::NotFound => newest_reset_backup(&legacy),
            _ => None,
        }
    });
    if let Some(backup) = backup {
        backup.publish(target);
    }
}

struct ResetBackup {
    path: PathBuf,
    unpublished: PathBuf,
}

/// 选择与发布分开：目录枚举结束后，其他准备者仍可能先把目标放回来。
fn newest_reset_backup(target: &Path) -> Option<ResetBackup> {
    let directory = target.parent()?;
    let name = target.file_name()?.to_str()?;
    let prefix = format!(".{name}.backup.");
    fs::read_dir(directory)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let stamp = entry
                .file_name()
                .to_str()?
                .strip_prefix(&prefix)?
                .to_owned();
            let order = stamp
                .parse::<u128>()
                .ok()
                .filter(|_| stamp.bytes().all(|byte| byte.is_ascii_digit()))?;
            is_real_file(&entry.path()).then_some((order, stamp, entry.path()))
        })
        .max_by_key(|(order, _, _)| *order)
        .map(|(_, stamp, path)| ResetBackup {
            path,
            unpublished: affixed(target, ".reset.", &stamp),
        })
}

impl ResetBackup {
    fn publish(self, target: &Path) {
        let Ok(mut backup) = tempfile::TempPath::try_from_path(self.path) else {
            return;
        };
        // 这是唯一的原件，失败或输掉发布竞争都不能让 `TempPath::drop` 删除它。
        backup.disable_cleanup(true);
        if backup.persist_noclobber(target).is_ok() {
            remove_ignoring_errors(&self.unpublished);
        }
    }
}

/// `.<name><infix><stamp>` beside `target`, built on the native string so a profile directory such as `C:\Users\陆傲天` survives unchanged; everything added is ASCII.
fn affixed(target: &Path, infix: &str, stamp: &str) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(target.file_name().unwrap_or_default());
    name.push(infix);
    name.push(stamp);
    target.with_file_name(name)
}

fn remove_ignoring_errors(path: &Path) {
    if path.is_dir() {
        let _ = fs::remove_dir_all(path);
    } else {
        let _ = fs::remove_file(path);
    }
}

fn is_real_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
}

fn reject_redirected_directory(path: &Path) -> std::io::Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if crate::paths::is_trusted_system_alias(ancestor) {
                    continue;
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "reset directory has a symbolic-link ancestor",
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotADirectory,
                    "reset directory parent is not a directory",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_dictionary::replay::tests::{sql, weight};

    /// 两个准备者先看到缺失目标；最新备份先发布后，迟到的旧备份不能覆盖它，也不能被失败路径清理。
    #[test]
    fn a_late_reset_recovery_preserves_the_winners_file_and_its_own_backup() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join(assets::USER_JOURNAL);
        let older = affixed(&target, ".backup.", "100");
        let newest = affixed(&target, ".backup.", "200");
        let older_stage = affixed(&target, ".reset.", "100");
        fs::write(&older, b"older synthetic journal").unwrap();
        fs::write(&newest, b"newest synthetic journal").unwrap();
        fs::write(&older_stage, b"unpublished synthetic journal").unwrap();
        assert_eq!(
            fs::symlink_metadata(&target).unwrap_err().kind(),
            ErrorKind::NotFound
        );

        newest_reset_backup(&target).unwrap().publish(&target);
        // 迟到者此前同样看到了缺失；枚举时最新备份已被赢家取走，只剩旧备份。
        newest_reset_backup(&target).unwrap().publish(&target);

        assert_eq!(fs::read(&target).unwrap(), b"newest synthetic journal");
        assert_eq!(fs::read(&older).unwrap(), b"older synthetic journal");
        assert_eq!(
            fs::read(&older_stage).unwrap(),
            b"unpublished synthetic journal"
        );
    }

    /// 现有词库沿用路径解析的优先级；两种名称都只有备份时优先恢复现名备份。
    #[test]
    fn reset_recovery_follows_current_and_legacy_name_priority() {
        for (name, legacy) in [
            (assets::MAIN_DICTIONARY, "msime.db"),
            (assets::ENGLISH_DICTIONARY, "english.db"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let paths = RuntimePaths {
                dictionaries: root.path().to_owned(),
                ..RuntimePaths::default()
            };
            let current = root.path().join(name);
            let old = root.path().join(legacy);
            let current_backup = affixed(&current, ".backup.", "100");
            let old_backup = affixed(&old, ".backup.", "200");
            fs::write(&current, b"existing synthetic dictionary").unwrap();
            fs::write(&old_backup, b"legacy synthetic backup").unwrap();
            restore_interrupted_reset(&paths.dictionary(name));
            assert_eq!(
                fs::read(&current).unwrap(),
                b"existing synthetic dictionary"
            );
            assert!(old_backup.exists());

            fs::rename(&current, &old).unwrap();
            fs::write(&current_backup, b"current synthetic backup").unwrap();
            restore_interrupted_reset(&paths.dictionary(name));
            assert_eq!(fs::read(&old).unwrap(), b"existing synthetic dictionary");
            assert!(current_backup.exists());
            assert!(!current.exists());

            fs::remove_file(&old).unwrap();
            restore_interrupted_reset(&paths.dictionary(name));
            assert_eq!(fs::read(&current).unwrap(), b"current synthetic backup");
            assert!(!current_backup.exists());
            assert_eq!(fs::read(&old_backup).unwrap(), b"legacy synthetic backup");
        }
    }

    fn paths(root: &Path) -> RuntimePaths {
        let path = |name: &str| {
            let path = root.join(name);
            fs::create_dir_all(&path).unwrap();
            path
        };
        RuntimePaths {
            resources: path("resources"),
            user_data: path("user"),
            cache: path("cache"),
            dictionaries: path("dictionaries"),
        }
    }

    /// 引擎层的重置自己取维护租约：还有会话持共享租约时报 `Busy`，词库和日志原样不动；会话结束后照常重置。
    #[test]
    fn reset_takes_the_maintenance_lease() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = paths(temporary.path());
        sql(
            &paths.resource(assets::MAIN_DICTIONARY),
            "CREATE TABLE tbl_2_n(key TEXT,jp TEXT,value TEXT,weight INTEGER);",
        );
        sql(
            &paths.resource(assets::ENGLISH_DICTIONARY),
            "CREATE TABLE english_words(word TEXT,display TEXT,weight INTEGER);",
        );
        fs::create_dir_all(&paths.user_data).unwrap();
        fs::create_dir_all(&paths.dictionaries).unwrap();
        let journal = paths.user(assets::USER_JOURNAL);
        sql(&journal, "CREATE TABLE marker(value INTEGER);");
        let session = crate::DictionaryAccess::try_session(&paths.user_data, &paths.dictionaries)
            .unwrap()
            .unwrap();

        let error = reset_learned_data(&paths).unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::Busy);
        let marker = "SELECT count(*) FROM sqlite_master WHERE name='marker'";
        assert_eq!(weight(&journal, marker), Some(1));
        assert!(!paths.dictionary(assets::MAIN_DICTIONARY).exists());

        drop(session);
        reset_learned_data(&paths).unwrap();
        assert!(paths.dictionary(assets::MAIN_DICTIONARY).exists());
        assert_eq!(weight(&journal, marker), Some(0));
    }

    /// 拆分发布布局：`msime-pinyin.db` 没有五笔表，重置后的工作主词库要把 `msime-wubi.db` 的码表并回来，五笔学习与删词才有表可写。
    #[test]
    fn reset_merges_the_split_wubi_tables_into_the_fresh_working_copy() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = paths(temporary.path());
        sql(
            &paths.resource(assets::MAIN_DICTIONARY),
            "CREATE TABLE tbl_2_n(key TEXT,jp TEXT,value TEXT,weight INTEGER);",
        );
        sql(
            &paths.resource(assets::WUBI_DICTIONARY),
            "CREATE TABLE wubi86(key TEXT,value TEXT,weight INTEGER);
             INSERT INTO wubi86 VALUES('aaaa','工',200);
             CREATE TABLE wubi98(key TEXT,value TEXT,weight INTEGER);",
        );
        sql(
            &paths.resource(assets::ENGLISH_DICTIONARY),
            "CREATE TABLE english_words(word TEXT,display TEXT,weight INTEGER);",
        );

        reset_learned_data(&paths).unwrap();

        assert_eq!(
            weight(
                &paths.dictionary(assets::MAIN_DICTIONARY),
                "SELECT weight FROM wubi86 WHERE key='aaaa'"
            ),
            Some(200)
        );
        assert_eq!(
            weight(
                &paths.dictionary(assets::MAIN_DICTIONARY),
                "SELECT count(*) FROM wubi98"
            ),
            Some(0)
        );
        assert!(!paths.dictionary(assets::WUBI_DICTIONARY).exists());
    }

    // engine-bridge tests.rs `reset_learned_data_restores_packaged_dictionaries_and_clears_journal`, for an ASCII root and one carrying Chinese characters.
    #[test]
    fn reset_restores_packaged_dictionaries_and_clears_the_journal() {
        for component in ["ascii", "陆傲天"] {
            let temporary = tempfile::tempdir().unwrap();
            let paths = paths(&temporary.path().join(component));
            sql(
                &paths.resource(assets::MAIN_DICTIONARY),
                "CREATE TABLE tbl_2_n(key TEXT,jp TEXT,value TEXT,weight INTEGER);
                 INSERT INTO tbl_2_n VALUES('ni''hao','nh','你好',100);
                 CREATE TABLE wubi86(key TEXT,value TEXT,weight INTEGER);
                 CREATE TABLE quick_parases(key TEXT,value TEXT,weight INTEGER);",
            );
            sql(
                &paths.resource(assets::ENGLISH_DICTIONARY),
                "CREATE TABLE english_words(word TEXT,display TEXT,weight INTEGER);
                 CREATE TABLE en_zh_glosses(english TEXT PRIMARY KEY,chinese_gloss TEXT);
                 CREATE TABLE zh_en_glosses(chinese TEXT PRIMARY KEY,english TEXT);
                 INSERT INTO english_words VALUES('word','word',100);",
            );
            for name in [assets::MAIN_DICTIONARY, assets::ENGLISH_DICTIONARY] {
                fs::copy(paths.resource(name), paths.dictionary(name)).unwrap();
            }
            let journal = paths.user(assets::USER_JOURNAL);
            // The legacy journal shape the bridge test used: no primary key, no `updated_at`.
            sql(
                &journal,
                "CREATE TABLE user_dictionary_operations(dictionary TEXT,key TEXT,value TEXT,operation TEXT,weight INTEGER,display TEXT,user_inserted INTEGER);
                 CREATE TABLE personal_dictionary_receipts(request_id TEXT PRIMARY KEY,payload TEXT);
                 CREATE TABLE candidate_selection_state(context_key TEXT,entry_key TEXT,value TEXT,selection_count INTEGER);
                 CREATE TABLE fixed_candidate_positions(context_key TEXT,entry_key TEXT,value TEXT,position INTEGER);
                 INSERT INTO user_dictionary_operations VALUES('pinyin','ni''hao','你好','upsert',1,'',1);
                 INSERT INTO candidate_selection_state VALUES('ni''hao','ni''hao','你好',7);",
            );
            sql(
                &paths.dictionary(assets::MAIN_DICTIONARY),
                "UPDATE tbl_2_n SET weight=1",
            );
            let mut wal = journal.as_os_str().to_owned();
            wal.push("-wal");
            fs::write(PathBuf::from(&wal), b"stale learned data").unwrap();

            reset_learned_data(&paths).unwrap();

            assert_eq!(
                weight(
                    &paths.dictionary(assets::MAIN_DICTIONARY),
                    "SELECT weight FROM tbl_2_n WHERE key='ni''hao'"
                ),
                Some(100)
            );
            assert_eq!(
                weight(&journal, "SELECT count(*) FROM user_dictionary_operations"),
                Some(0)
            );
            assert_eq!(
                weight(&journal, "SELECT count(*) FROM candidate_selection_state"),
                Some(0)
            );
            // The fresh journal has the full v4 schema.
            assert_eq!(
                weight(&journal, "SELECT count(*) FROM pinned_candidates"),
                Some(0)
            );
            assert!(!PathBuf::from(&wal).exists());
            let leftovers: Vec<_> = [&paths.user_data, &paths.dictionaries]
                .iter()
                .flat_map(|directory| fs::read_dir(directory).unwrap())
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                // 维护租约的锁文件是稳定的协调对象，留着是对的（`access::LOCK_FILE`）。
                .filter(|name| name.starts_with('.') && name != crate::access::LOCK_FILE)
                .collect();
            assert!(leftovers.is_empty(), "{leftovers:?}");
        }
    }

    /// 重置学习数据连双拼按两键码学的纠错（#10018）一起清掉：新日志里两张表是空的，共用的纠错档案下次刷新就读不到旧记录。
    #[test]
    fn reset_clears_shuangpin_typo_learning() {
        use crate::types::ShuangpinProfileKind;
        use crate::user_dictionary::typo_profile::{PersonalTypoProfile, ShuangpinTypoOutcome};
        let temporary = tempfile::tempdir().unwrap();
        let paths = paths(temporary.path());
        sql(
            &paths.resource(assets::MAIN_DICTIONARY),
            "CREATE TABLE tbl_2_n(key TEXT,jp TEXT,value TEXT,weight INTEGER);",
        );
        sql(
            &paths.resource(assets::ENGLISH_DICTIONARY),
            "CREATE TABLE english_words(word TEXT,display TEXT,weight INTEGER);",
        );
        let journal = paths.user(assets::USER_JOURNAL);
        let profile = PersonalTypoProfile::shared(&journal);
        profile
            .record_shuangpin(
                ShuangpinProfileKind::Xiaohe,
                &[(*b"gf", *b"gr")],
                ShuangpinTypoOutcome::Accepted,
            )
            .unwrap();
        profile
            .record_shuangpin_suppression(ShuangpinProfileKind::Xiaohe, "mwgfxi")
            .unwrap();
        assert!(profile.has_shuangpin_counts());

        reset_learned_data(&paths).unwrap();

        for table in [
            "shuangpin_typo_counts",
            "shuangpin_autocorrect_suppressions",
        ] {
            assert_eq!(
                weight(&journal, &format!("SELECT count(*) FROM {table}")),
                Some(0),
                "{table}"
            );
        }
        profile.refresh_if_changed();
        assert!(!profile.has_shuangpin_counts());
        assert!(!profile.shuangpin_suppressed(ShuangpinProfileKind::Xiaohe, "mwgfxi"));
    }

    #[test]
    fn reset_refuses_the_bundle_in_place_and_a_missing_package() {
        let root = tempfile::tempdir().unwrap();
        let mut paths = paths(root.path());
        let error = reset_learned_data(&paths).unwrap_err();
        assert_eq!(
            error.to_string(),
            diagnostics::PACKAGED_DICTIONARY_UNAVAILABLE
        );
        paths.dictionaries = paths.resources.join(".");
        let error = reset_learned_data(&paths).unwrap_err();
        assert!(matches!(error, EngineError::InvalidArgument(_)));
        assert_eq!(error.to_string(), diagnostics::RESET_IN_PLACE);
    }

    /// 没有 `msime-pinyin.db` 的代次：重置只换回 `msime-english.db`、清空日志，不要求资源目录里有 `msime-pinyin.db`，也不在代次里留下它；同一个资源目录按有 `msime-pinyin.db` 重置仍然被拒。
    #[test]
    fn reset_without_the_main_dictionary_restores_only_english() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        sql(
            &paths.resource(assets::ENGLISH_DICTIONARY),
            "CREATE TABLE english_words(word TEXT,display TEXT,weight INTEGER);
             INSERT INTO english_words VALUES('word','word',100);",
        );
        fs::copy(
            paths.resource(assets::ENGLISH_DICTIONARY),
            paths.dictionary(assets::ENGLISH_DICTIONARY),
        )
        .unwrap();
        sql(
            &paths.dictionary(assets::ENGLISH_DICTIONARY),
            "UPDATE english_words SET weight=1",
        );
        let journal = paths.user(assets::USER_JOURNAL);
        sql(
            &journal,
            "CREATE TABLE user_dictionary_operations(dictionary TEXT,key TEXT,value TEXT,operation TEXT,weight INTEGER,display TEXT,user_inserted INTEGER);
             INSERT INTO user_dictionary_operations VALUES('english','word','word','upsert',1,'word',0);",
        );

        assert_eq!(
            reset_learned_data(&paths).unwrap_err().to_string(),
            diagnostics::PACKAGED_DICTIONARY_UNAVAILABLE
        );
        reset_learned_data_with(&paths, false).unwrap();

        assert_eq!(
            weight(
                &paths.dictionary(assets::ENGLISH_DICTIONARY),
                "SELECT weight FROM english_words WHERE word='word'"
            ),
            Some(100)
        );
        assert_eq!(
            weight(&journal, "SELECT count(*) FROM user_dictionary_operations"),
            Some(0)
        );
        assert!(!paths.dictionary(assets::MAIN_DICTIONARY).exists());
        assert!(!paths.resource(assets::MAIN_DICTIONARY).exists());
        let leftovers: Vec<_> = [&paths.user_data, &paths.dictionaries]
            .iter()
            .flat_map(|directory| fs::read_dir(directory).unwrap())
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            // 维护租约的锁文件是稳定的协调对象，留着是对的（`access::LOCK_FILE`）。
            .filter(|name| name.starts_with('.') && name != crate::access::LOCK_FILE)
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn reset_rejects_a_symlinked_packaged_dictionary() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        sql(
            &paths.resource(assets::ENGLISH_DICTIONARY),
            "CREATE TABLE english_words(word TEXT,display TEXT,weight INTEGER);",
        );
        let external = root.path().join("external-msime-pinyin.db");
        sql(
            &external,
            "CREATE TABLE tbl_2_n(key TEXT,jp TEXT,value TEXT,weight INTEGER);",
        );
        symlink(&external, paths.resource(assets::MAIN_DICTIONARY)).unwrap();

        assert_eq!(
            reset_learned_data(&paths).unwrap_err().to_string(),
            diagnostics::PACKAGED_DICTIONARY_UNAVAILABLE
        );
        assert!(!paths.dictionary(assets::MAIN_DICTIONARY).exists());
    }

    #[cfg(unix)]
    #[test]
    fn reset_rejects_a_symlinked_dictionary_directory() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        for name in [assets::MAIN_DICTIONARY, assets::ENGLISH_DICTIONARY] {
            sql(&paths.resource(name), "CREATE TABLE packaged(x);");
        }
        fs::remove_dir(&paths.dictionaries).unwrap();
        symlink(outside.path(), &paths.dictionaries).unwrap();

        assert!(reset_learned_data(&paths).is_err());
        assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
    }

    #[test]
    fn a_failed_publish_puts_every_original_back() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        sql(
            &paths.resource(assets::MAIN_DICTIONARY),
            "CREATE TABLE fresh(x);",
        );
        sql(
            &paths.resource(assets::ENGLISH_DICTIONARY),
            "CREATE TABLE fresh(x);",
        );
        sql(
            &paths.dictionary(assets::MAIN_DICTIONARY),
            "CREATE TABLE learned(x);",
        );
        sql(
            &paths.dictionary(assets::ENGLISH_DICTIONARY),
            "CREATE TABLE learned(x);",
        );
        let journal = paths.user(assets::USER_JOURNAL);
        sql(&journal, "CREATE TABLE learned(x);");
        // A `-wal` sidecar that is a directory cannot be removed as a file, so the reset fails after all three files were published.
        let mut wal = journal.as_os_str().to_owned();
        wal.push("-wal");
        let wal = PathBuf::from(wal);
        fs::create_dir_all(wal.join("blocker")).unwrap();
        let before: Vec<Vec<u8>> = [
            paths.dictionary(assets::MAIN_DICTIONARY),
            paths.dictionary(assets::ENGLISH_DICTIONARY),
            journal.clone(),
        ]
        .iter()
        .map(|path| fs::read(path).unwrap())
        .collect();

        assert!(reset_learned_data(&paths).is_err());

        let after: Vec<Vec<u8>> = [
            paths.dictionary(assets::MAIN_DICTIONARY),
            paths.dictionary(assets::ENGLISH_DICTIONARY),
            journal.clone(),
        ]
        .iter()
        .map(|path| fs::read(path).unwrap())
        .collect();
        assert_eq!(after, before);
        assert!(wal.join("blocker").is_dir());
        let leftovers: Vec<_> = [&paths.user_data, &paths.dictionaries]
            .iter()
            .flat_map(|directory| fs::read_dir(directory).unwrap())
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            // 维护租约的锁文件是稳定的协调对象，留着是对的（`access::LOCK_FILE`）。
            .filter(|name| name.starts_with('.') && name != crate::access::LOCK_FILE)
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn reset_auto_learned_data_clears_learned_state_and_caches_while_preserving_manual_entries() {
        use crate::lattice::personal::PersonalTransition;
        use crate::types::ShuangpinProfileKind;
        use crate::user_dictionary::ngram_store::{flush_all, PersonalNgramStore};
        use crate::user_dictionary::typo_profile::{PersonalTypoProfile, ShuangpinTypoOutcome};

        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        sql(
            &paths.resource(assets::MAIN_DICTIONARY),
            "CREATE TABLE tbl_2_n(key TEXT,jp TEXT,value TEXT,weight INTEGER);
             INSERT INTO tbl_2_n VALUES('ni''hao','nh','你好',200),('ni''hao','nh','拟好',100);
             CREATE TABLE tbl_4_f(key TEXT,jp TEXT,value TEXT,weight INTEGER);
             CREATE TABLE wubi86(key TEXT,value TEXT,weight INTEGER);",
        );
        sql(
            &paths.resource(assets::ENGLISH_DICTIONARY),
            "CREATE TABLE english_words(word TEXT,display TEXT,weight INTEGER);
             INSERT INTO english_words VALUES('hello','hello',100);",
        );
        for name in [assets::MAIN_DICTIONARY, assets::ENGLISH_DICTIONARY] {
            fs::copy(paths.resource(name), paths.dictionary(name)).unwrap();
        }

        let journal = paths.user(assets::USER_JOURNAL);
        sql(
            &journal,
            "CREATE TABLE user_dictionary_operations(
                dictionary TEXT NOT NULL,key TEXT NOT NULL,value TEXT NOT NULL,
                operation TEXT NOT NULL CHECK(operation IN ('upsert','delete')),
                weight INTEGER NOT NULL DEFAULT 0,display TEXT NOT NULL DEFAULT '',
                user_inserted INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL DEFAULT(unixepoch()),
                backup_timestamp REAL,
                PRIMARY KEY(dictionary,key,value));
             CREATE TABLE responsay_hotwords(
                term TEXT PRIMARY KEY NOT NULL,
                updated_at INTEGER NOT NULL,
                deleted INTEGER NOT NULL DEFAULT 0,
                weight INTEGER NOT NULL DEFAULT 1000,
                manual INTEGER NOT NULL DEFAULT 1);
             INSERT INTO responsay_hotwords(term,updated_at,deleted,weight,manual)
                VALUES('法言热词',1700000001,0,1000,1),('自动热词',1700000002,0,100,0);
             INSERT INTO user_dictionary_operations(dictionary,key,value,operation,weight,display,user_inserted,updated_at,backup_timestamp)
                VALUES
                ('pinyin','fa''yan''shu''ru','法言输入','upsert',1000,'',1,1700000003,1700000003.5),
                ('english','responsay','Responsay','upsert',1000,'Responsay',1,1700000004,1700000004.25),
                ('pinyin','ni''hao','拟好','upsert',250,'',0,1700000005,NULL),
                ('english','autoword','AutoWord','upsert',10,'AutoWord',0,1700000006,NULL);",
        );
        sql(
            &paths.dictionary(assets::MAIN_DICTIONARY),
            "UPDATE tbl_2_n SET weight=250 WHERE value='拟好';
             INSERT INTO tbl_2_n VALUES('zi''zao','zz','自造',10000);",
        );
        sql(
            &paths.dictionary(assets::ENGLISH_DICTIONARY),
            "INSERT INTO english_words VALUES('autoword','AutoWord',10);",
        );

        let ngram_store = PersonalNgramStore::for_journal(&journal);
        ngram_store
            .record(&[PersonalTransition {
                earlier: String::new(),
                previous: "你好".to_owned(),
                word: "世界".to_owned(),
                times: 4,
            }])
            .unwrap();
        flush_all();
        assert!(!ngram_store.model().is_empty());

        let typo_profile = PersonalTypoProfile::shared(&journal);
        typo_profile
            .record_accepted(&[("gan".to_owned(), "guan".to_owned())])
            .unwrap();
        typo_profile.record_suppression("zhogn").unwrap();
        typo_profile
            .record_shuangpin(
                ShuangpinProfileKind::Xiaohe,
                &[(*b"gf", *b"gr")],
                ShuangpinTypoOutcome::Accepted,
            )
            .unwrap();
        assert_eq!(typo_profile.accepted("gan", "guan"), 1);
        assert!(typo_profile.suppressed("zhogn"));
        assert!(typo_profile.has_shuangpin_counts());

        sql(
            &journal,
            "INSERT INTO candidate_selection_state VALUES('ni''hao','ni''hao','拟好',3);
             INSERT INTO fixed_candidate_positions VALUES('nine-key:64:','ni','你',1);
             INSERT INTO pick_transitions VALUES('ni','你','hao','好',2,100);
             INSERT INTO pinned_candidates VALUES('ni''hao','拟好',100);
             INSERT INTO quick_phrase_slots VALUES('rq',2);",
        );

        reset_auto_learned_data(&paths).unwrap();

        assert!(ngram_store.model().is_empty());
        assert_eq!(typo_profile.accepted("gan", "guan"), 0);
        assert!(!typo_profile.suppressed("zhogn"));
        assert!(!typo_profile.has_shuangpin_counts());

        for table in [
            "candidate_selection_state",
            "fixed_candidate_positions",
            "pick_transitions",
            "pinyin_typo_counts",
            "pinyin_autocorrect_suppressions",
            "shuangpin_typo_counts",
            "shuangpin_autocorrect_suppressions",
            "pinned_candidates",
            "personal_bigram",
            "personal_trigram",
            "quick_phrase_slots",
        ] {
            assert_eq!(
                weight(&journal, &format!("SELECT count(*) FROM {table}")),
                Some(0),
                "{table}"
            );
        }

        assert_eq!(
            weight(
                &journal,
                "SELECT count(*) FROM user_dictionary_operations WHERE user_inserted=0",
            ),
            Some(0)
        );
        assert_eq!(
            weight(
                &journal,
                "SELECT count(*) FROM user_dictionary_operations WHERE user_inserted=1",
            ),
            Some(2)
        );
        assert_eq!(
            weight(
                &journal,
                "SELECT CAST(backup_timestamp * 10 AS INTEGER) FROM user_dictionary_operations WHERE value='法言输入'",
            ),
            Some(17_000_000_035)
        );
        assert_eq!(
            weight(
                &journal,
                "SELECT count(*) FROM responsay_hotwords WHERE term='法言热词' AND manual=1 AND deleted=0",
            ),
            Some(1)
        );
        assert_eq!(
            weight(
                &journal,
                "SELECT count(*) FROM responsay_hotwords WHERE manual=0",
            ),
            Some(0)
        );

        assert_eq!(
            weight(
                &paths.dictionary(assets::MAIN_DICTIONARY),
                "SELECT weight FROM tbl_2_n WHERE value='拟好'",
            ),
            Some(100)
        );
        assert_eq!(
            weight(
                &paths.dictionary(assets::MAIN_DICTIONARY),
                "SELECT count(*) FROM tbl_2_n WHERE value='自造'",
            ),
            Some(0)
        );
        assert_eq!(
            weight(
                &paths.dictionary(assets::MAIN_DICTIONARY),
                "SELECT weight FROM tbl_4_f WHERE key='fa''yan''shu''ru' AND value='法言输入'",
            ),
            Some(1000)
        );
        assert_eq!(
            weight(
                &paths.dictionary(assets::ENGLISH_DICTIONARY),
                "SELECT count(*) FROM english_words WHERE word='autoword'",
            ),
            Some(0)
        );
        assert_eq!(
            weight(
                &paths.dictionary(assets::ENGLISH_DICTIONARY),
                "SELECT weight FROM english_words WHERE word='responsay' AND display='Responsay'",
            ),
            Some(1000)
        );
    }

    #[test]
    fn reset_auto_learned_journal_before_generation_is_ready_leaves_clean_generation_for_prepare() {
        use crate::prepare_runtime_paths;
        use crate::types::PersonalDictionaryKind;
        use crate::user_dictionary::journal;

        let root = tempfile::tempdir().unwrap();
        let base = paths(root.path());
        sql(
            &base.resource(assets::MAIN_DICTIONARY),
            "CREATE TABLE tbl_2_z(key TEXT,jp TEXT,value TEXT,weight INTEGER);
             CREATE TABLE tbl_4_f(key TEXT,jp TEXT,value TEXT,weight INTEGER);
             CREATE TABLE wubi86(key TEXT,value TEXT,weight INTEGER);",
        );
        sql(
            &base.resource(assets::ENGLISH_DICTIONARY),
            "CREATE TABLE english_words(word TEXT,display TEXT,weight INTEGER);
             INSERT INTO english_words VALUES('hello','hello',100);",
        );
        let journal = base.user(assets::USER_JOURNAL);
        journal::record_learned_insert(
            &journal,
            PersonalDictionaryKind::Pinyin,
            "zi'zao",
            "自造",
            300,
            "",
            false,
        )
        .unwrap();
        journal::record_learned_insert(
            &journal,
            PersonalDictionaryKind::Pinyin,
            "fa'yan'shu'ru",
            "法言输入",
            1000,
            "",
            true,
        )
        .unwrap();

        reset_auto_learned_journal(&base.user_data).unwrap();

        let prepared =
            prepare_runtime_paths(&base.resources, &base.user_data, &base.cache, "gen-1").unwrap();
        assert_eq!(
            weight(
                &prepared.dictionary(assets::MAIN_DICTIONARY),
                "SELECT count(*) FROM tbl_2_z WHERE value='自造'",
            ),
            Some(0)
        );
        assert_eq!(
            weight(
                &prepared.dictionary(assets::MAIN_DICTIONARY),
                "SELECT weight FROM tbl_4_f WHERE key='fa''yan''shu''ru' AND value='法言输入'",
            ),
            Some(1000)
        );
    }
}
