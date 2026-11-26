// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL was not distributed with this file, You can obtain one at http://mozilla.org/MPL/2.0/.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use msime_engine::touch::{KeyGeometry, KeyRect};
use msime_engine::{
    autocorrect_type, Clock, Command, FrequencyAdjustmentMode, FrequencyAdjustmentOptions,
    LocalDateTime, RuntimePaths, SchemeSet, SchemeType, Session, SessionOptions,
    ShuangpinProfileKind,
};
use serde::Deserialize;

fn sibling_ios_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../responsay-ios")
}

fn lexicon_dir() -> Option<PathBuf> {
    if let Some(env_dir) = std::env::var_os("MSIME_TEST_LEXICON_DIR") {
        let dir = PathBuf::from(env_dir);
        if dir.join("msime-pinyin.db").is_file() {
            return Some(dir);
        }
    }
    let dir = sibling_ios_root().join("build/Lexicon");
    dir.join("msime-pinyin.db").is_file().then_some(dir)
}

fn make_iphone_geometry(width: f32) -> (Arc<KeyGeometry>, [KeyRect; 26]) {
    let row_h = 45.0f32;
    let row_spacing = 11.0f32;
    let outer_pad = 6.5f32;
    let key_spacing = 6.0f32;
    let plane_w = width - outer_pad * 2.0;
    let unit_w = (plane_w - key_spacing * 9.0) / 10.0;
    let total_w1 = 9.0 * unit_w + 8.0 * key_spacing;
    let start_x1 = outer_pad + (plane_w - total_w1) * 0.5;
    let middle_w = 7.0 * unit_w + 6.0 * key_spacing;
    let side_space = (plane_w - middle_w) * 0.5;

    let row0 = b"qwertyuiop";
    let row1 = b"asdfghjkl";
    let row2 = b"zxcvbnm";

    let mut rects = [[0.0f32; 4]; 26];
    for (col, &ch) in row0.iter().enumerate() {
        let cx = outer_pad + (col as f32) * (unit_w + key_spacing) + unit_w * 0.5;
        let cy = row_h * 0.5;
        rects[(ch - b'a') as usize] = [cx, cy, unit_w, row_h];
    }
    for (col, &ch) in row1.iter().enumerate() {
        let cx = start_x1 + (col as f32) * (unit_w + key_spacing) + unit_w * 0.5;
        let cy = (row_h + row_spacing) + row_h * 0.5;
        rects[(ch - b'a') as usize] = [cx, cy, unit_w, row_h];
    }
    for (col, &ch) in row2.iter().enumerate() {
        let cx = outer_pad + side_space + (col as f32) * (unit_w + key_spacing) + unit_w * 0.5;
        let cy = 2.0 * (row_h + row_spacing) + row_h * 0.5;
        rects[(ch - b'a') as usize] = [cx, cy, unit_w, row_h];
    }
    let geo = Arc::new(KeyGeometry::new(1, &rects, None, &[]).expect("geometry"));
    (geo, rects)
}

fn build_ios_session(lexicon: &Path, user_dir: &Path, autocorrect: u32) -> Session {
    let paths = RuntimePaths {
        resources: lexicon.to_path_buf(),
        user_data: user_dir.to_path_buf(),
        cache: user_dir.to_path_buf(),
        dictionaries: lexicon.to_path_buf(),
    };
    let mut options = SessionOptions::new(paths);
    options.enabled_schemes = SchemeSet::of(&[SchemeType::Quanpin, SchemeType::Shuangpin]);
    options.shuangpin_profile = ShuangpinProfileKind::Xiaohe;
    options.learning = false;
    options.personal_context = false;
    options.learn_english_on_enter = false;
    options.next_word_prediction = false;
    options.first_page_rows = 24;
    options.sentence_alternatives = false;
    options.mixed_candidates = true;
    options.english.mixed_candidates = true;
    options.english.minimum_prefix = 5;
    options.english.first_page_count = 1;
    options.quick_phrase_candidates = true;
    options.local_modes.unicode = false;
    options.local_modes.date_time = false;
    options.local_modes.date_time_inline = true;
    options.local_modes.date_time_menu = true;
    options.local_modes.quick_phrase = false;
    options.local_modes.emoji = false;
    options.local_modes.kaomoji = false;
    options.local_modes.super_jianpin = false;
    options.local_modes.temporary_english = false;
    options.local_modes.temporary_japanese = false;
    options.local_modes.expression = false;
    options.local_modes.command = false;
    options.local_modes.mention = false;
    options.local_modes.chaizi = false;
    options.local_modes.symbol = false;
    options.local_modes.shift_letter_entry = true;
    options.autocorrect_types = autocorrect;
    options.frequency = FrequencyAdjustmentOptions {
        mode: FrequencyAdjustmentMode::Disabled,
        trigger_count: 1,
        linear_step: 1,
    };
    let mut session = Session::new(options).expect("session");
    session.set_clock(Clock {
        local: Arc::new(|| LocalDateTime {
            year: 2026,
            month: 11,
            day: 26,
            weekday: 4,
            hour: 10,
            minute: 30,
            second: 0,
        }),
        ..Clock::default()
    });
    session
}

#[derive(Deserialize)]
struct TapEvent {
    tapped_key: String,
    neighbor_key: String,
    blend: f32,
}

#[derive(Deserialize)]
struct SlipCase {
    id: String,
    slip_kind: String,
    raw_input: String,
    expected: String,
    taps: Vec<TapEvent>,
}

#[derive(Deserialize)]
struct BenchCase {
    id: String,
    input: String,
    expected: String,
}

#[derive(Deserialize)]
struct SharedBenchCorpus {
    quanpin_standard: Vec<BenchCase>,
    long_colloquial_homophone: Vec<BenchCase>,
    blind_typing_slips: Vec<SlipCase>,
    mid_sentence_cn_en: Vec<BenchCase>,
}

#[derive(Deserialize)]
struct LexiconEvalEntry {
    id: String,
    scheme: String,
    input: String,
    expected: String,
    category: String,
    mixed_english_candidates: Option<bool>,
}

#[test]
fn blind_slip_dual_gate_and_zero_clean_regressions() {
    let Some(lexicon) = lexicon_dir() else {
        return;
    };
    let Some(bench_path) = std::env::var_os("MSIME_TEST_SHARED_BENCH_CORPUS")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
    else {
        return;
    };
    let eval_path = sibling_ios_root()
        .join("Tests/ResponsayKeyboardCoreTests/Typing/Fixtures/lexicon-eval-corpus.json");
    if !eval_path.is_file() {
        return;
    }

    let bench: SharedBenchCorpus =
        serde_json::from_slice(&std::fs::read(&bench_path).unwrap()).unwrap();
    let eval: Vec<LexiconEvalEntry> =
        serde_json::from_slice(&std::fs::read(&eval_path).unwrap()).unwrap();

    let ac_all = autocorrect_type::TRANSPOSITION
        | autocorrect_type::NEIGHBOR
        | autocorrect_type::MISSING_OR_EXTRA;
    let temp = tempfile::tempdir().unwrap();
    let mut flat_session = build_ios_session(&lexicon, temp.path(), ac_all);
    let mut geo_flat_session = build_ios_session(&lexicon, temp.path(), ac_all);
    let mut touched_session = build_ios_session(&lexicon, temp.path(), ac_all);
    let (geo, rects) = make_iphone_geometry(393.0);
    geo_flat_session.set_key_geometry(Some(Arc::clone(&geo)));
    touched_session.set_key_geometry(Some(Arc::clone(&geo)));

    let mut slip_flat_top1 = 0usize;
    let mut slip_touch_top1 = 0usize;
    for case in &bench.blind_typing_slips {
        let _ = &case.slip_kind;
        for b in case.raw_input.bytes() {
            flat_session.character(b, false);
            geo_flat_session.character(b, false);
        }
        let flat_cands: Vec<String> = flat_session
            .snapshot()
            .candidates
            .iter()
            .take(5)
            .map(|c| c.word.to_string())
            .collect();
        let geo_flat_cands: Vec<String> = geo_flat_session
            .snapshot()
            .candidates
            .iter()
            .take(5)
            .map(|c| c.word.to_string())
            .collect();
        flat_session.command(Command::Cancel);
        geo_flat_session.command(Command::Cancel);
        assert_eq!(flat_cands, geo_flat_cands, "{} geo_flat parity", case.id);

        for tap in &case.taps {
            let b = tap.tapped_key.as_bytes()[0];
            let nb = tap.neighbor_key.as_bytes()[0];
            let r_tap = rects[(b - b'a') as usize];
            let r_nb = rects[(nb - b'a') as usize];
            let x = r_tap[0] + (r_nb[0] - r_tap[0]) * tap.blend;
            let y = r_tap[1] + (r_nb[1] - r_tap[1]) * tap.blend;
            touched_session.set_key_touch(geo.touch(b, x, y));
            touched_session.character(b, false);
        }
        let touch_cands: Vec<String> = touched_session
            .snapshot()
            .candidates
            .iter()
            .take(5)
            .map(|c| c.word.to_string())
            .collect();
        touched_session.command(Command::Cancel);

        let flat_rank = flat_cands
            .iter()
            .position(|c| c == &case.expected)
            .map(|i| i + 1);
        let touch_rank = touch_cands
            .iter()
            .position(|c| c == &case.expected)
            .map(|i| i + 1);
        if flat_rank == Some(1) {
            slip_flat_top1 += 1;
        }
        if touch_rank == Some(1) {
            slip_touch_top1 += 1;
        }
        if matches!(case.id.as_str(), "c05" | "c08" | "c15") {
            assert_eq!(flat_rank, Some(1), "{} flat must be #1", case.id);
            assert_eq!(touch_rank, Some(1), "{} touch must be #1", case.id);
        }
    }
    assert!(slip_flat_top1 >= 11, "slip_flat_top1={slip_flat_top1}");
    assert!(slip_touch_top1 >= 13, "slip_touch_top1={slip_touch_top1}");

    let jitters: [(f32, f32); 6] = [
        (0.22, -0.18),
        (-0.21, 0.19),
        (0.18, 0.22),
        (-0.23, -0.17),
        (0.24, 0.0),
        (0.0, -0.24),
    ];

    let mut check_clean_subset = |name: &str,
                                  cases: &[(String, String, String)],
                                  baseline_min_top1: usize| {
        let mut flat_top1 = 0usize;
        let mut center_flips = Vec::new();
        let mut jitter_flips = Vec::new();

        for (case_idx, (id, input, expected)) in cases.iter().enumerate() {
            for b in input.bytes() {
                flat_session.character(b, false);
                geo_flat_session.character(b, false);
            }
            let f_top = flat_session
                .snapshot()
                .candidates
                .first()
                .map(|c| c.word.to_string());
            let gf_top = geo_flat_session
                .snapshot()
                .candidates
                .first()
                .map(|c| c.word.to_string());
            flat_session.command(Command::Cancel);
            geo_flat_session.command(Command::Cancel);
            assert_eq!(f_top, gf_top, "{id}({input}) flat vs geo_flat");
            if f_top.as_deref() == Some(expected.as_str()) {
                flat_top1 += 1;
            }

            if input.bytes().all(|b| b.is_ascii_lowercase()) {
                for b in input.bytes() {
                    let r = rects[(b - b'a') as usize];
                    touched_session.set_key_touch(geo.touch(b, r[0], r[1]));
                    touched_session.character(b, false);
                }
                let c_top = touched_session
                    .snapshot()
                    .candidates
                    .first()
                    .map(|c| c.word.to_string());
                touched_session.command(Command::Cancel);
                if c_top != f_top {
                    center_flips.push(format!("{id}({input}): flat={f_top:?} center={c_top:?}"));
                }

                for (ch_idx, b) in input.bytes().enumerate() {
                    let r = rects[(b - b'a') as usize];
                    let (jx, jy) = jitters[(case_idx + ch_idx) % jitters.len()];
                    let pt_x = r[0] + jx * r[2];
                    let pt_y = r[1] + jy * r[3];
                    touched_session.set_key_touch(geo.touch(b, pt_x, pt_y));
                    touched_session.character(b, false);
                }
                let j_top = touched_session
                    .snapshot()
                    .candidates
                    .first()
                    .map(|c| c.word.to_string());
                touched_session.command(Command::Cancel);
                if j_top != f_top {
                    jitter_flips.push(format!("{id}({input}): flat={f_top:?} jitter={j_top:?}"));
                }
            }
        }
        println!(
            "corpus {name}: top1={flat_top1}/{} center_flips={} jitter_flips={}",
            cases.len(),
            center_flips.len(),
            jitter_flips.len()
        );
        assert!(
            flat_top1 >= baseline_min_top1,
            "{name} top1 {flat_top1} < baseline {baseline_min_top1}"
        );
        assert!(
            center_flips.is_empty(),
            "{name} center_flips: {center_flips:?}"
        );
        assert!(
            jitter_flips.is_empty(),
            "{name} jitter_flips: {jitter_flips:?}"
        );
    };

    let bench_a: Vec<_> = bench
        .quanpin_standard
        .iter()
        .map(|c| (c.id.clone(), c.input.clone(), c.expected.clone()))
        .collect();
    let bench_b: Vec<_> = bench
        .long_colloquial_homophone
        .iter()
        .map(|c| (c.id.clone(), c.input.clone(), c.expected.clone()))
        .collect();
    let bench_d: Vec<_> = bench
        .mid_sentence_cn_en
        .iter()
        .map(|c| (c.id.clone(), c.input.clone(), c.expected.clone()))
        .collect();
    let eval_qp: Vec<_> = eval
        .iter()
        .filter(|e| e.scheme == "quanpin" && e.mixed_english_candidates.unwrap_or(true))
        .map(|e| {
            let _ = &e.category;
            (e.id.clone(), e.input.clone(), e.expected.clone())
        })
        .collect();

    check_clean_subset("bench.quanpin_standard", &bench_a, 189);
    check_clean_subset("bench.long_colloquial_homophone", &bench_b, 18);
    check_clean_subset("bench.mid_sentence_cn_en", &bench_d, 14);
    check_clean_subset("lexicon_eval.quanpin", &eval_qp, 211);

    let mut eval_top1 = 0usize;
    let mut eval_top3 = 0usize;
    for entry in &eval {
        let mixed = entry.mixed_english_candidates.unwrap_or(true);
        flat_session.command(Command::Cancel);
        flat_session.reset_context();
        flat_session.set_english_mixed_candidates(mixed);
        match entry.scheme.as_str() {
            "quanpin" => {
                flat_session.set_dedicated_english(false);
                flat_session.switch_scheme(SchemeType::Quanpin).unwrap();
            }
            "shuangpin" => {
                flat_session.set_dedicated_english(false);
                flat_session.switch_scheme(SchemeType::Shuangpin).unwrap();
            }
            "english" => {
                flat_session.switch_scheme(SchemeType::Quanpin).unwrap();
                flat_session.set_dedicated_english(true);
            }
            other => panic!("unexpected scheme {other}"),
        }
        for b in entry.input.bytes() {
            flat_session.character(b, false);
        }
        if flat_session.snapshot().candidates.len() < 24 {
            flat_session.expand_initial_candidates();
        }
        let cands: Vec<String> = flat_session
            .snapshot()
            .candidates
            .iter()
            .take(24)
            .map(|c| c.word.to_string())
            .collect();
        let rank = cands
            .iter()
            .position(|w| w == &entry.expected)
            .map(|i| i + 1);
        if rank == Some(1) {
            eval_top1 += 1;
        }
        if rank.is_some_and(|r| r <= 3) {
            eval_top3 += 1;
        }
    }
    println!(
        "corpus lexicon_eval.all: top1={eval_top1}/{} top3={eval_top3}/{} slip_flat_top1={slip_flat_top1}/15 slip_touch_top1={slip_touch_top1}/15",
        eval.len(),
        eval.len()
    );
    assert!(
        eval_top1 >= 236,
        "lexicon_eval.all top1 {eval_top1} < baseline 236"
    );
}

#[test]
fn deferred_burst_jintianxiawu_matches_normal_typing_100_percent() {
    let Some(lexicon) = lexicon_dir() else {
        return;
    };
    let ac_all = autocorrect_type::TRANSPOSITION
        | autocorrect_type::NEIGHBOR
        | autocorrect_type::MISSING_OR_EXTRA;
    let temp = tempfile::tempdir().unwrap();
    let (geo, rects) = make_iphone_geometry(393.0);

    for use_touch in [false, true] {
        let mut normal = build_ios_session(&lexicon, temp.path(), ac_all);
        let mut partial_defer = build_ios_session(&lexicon, temp.path(), ac_all);
        let mut full_defer = build_ios_session(&lexicon, temp.path(), ac_all);
        if use_touch {
            normal.set_key_geometry(Some(Arc::clone(&geo)));
            partial_defer.set_key_geometry(Some(Arc::clone(&geo)));
            full_defer.set_key_geometry(Some(Arc::clone(&geo)));
        }

        let input = b"jintianxiawu";
        for (idx, &b) in input.iter().enumerate() {
            if use_touch {
                let r = rects[(b - b'a') as usize];
                normal.set_key_touch(geo.touch(b, r[0], r[1]));
                partial_defer.set_key_touch(geo.touch(b, r[0], r[1]));
                full_defer.set_key_touch(geo.touch(b, r[0], r[1]));
            }
            normal.character_with_defer(b, false, false);
            partial_defer.character_with_defer(b, false, idx < 11);
            full_defer.character_with_defer(b, false, true);
            if idx < 11 {
                assert!(partial_defer.has_deferred_candidates());
                assert!(partial_defer
                    .deferred_display_snapshot()
                    .candidates
                    .is_empty());
                assert_eq!(
                    partial_defer.deferred_display_snapshot().editing_text.len(),
                    idx + 1
                );
            }
        }

        assert!(!partial_defer.has_deferred_candidates());
        assert!(full_defer.has_deferred_candidates());

        let normal_snap = normal.display_snapshot();
        let partial_snap = partial_defer.display_snapshot();
        assert_eq!(partial_snap.preedit, normal_snap.preedit);
        assert_eq!(partial_snap.editing_text, normal_snap.editing_text);
        assert_eq!(partial_snap.caret_position, normal_snap.caret_position);
        assert_eq!(partial_snap.candidates, normal_snap.candidates);

        let normal_commit = normal.command(Command::CommitCandidate);
        let partial_commit = partial_defer.command(Command::CommitCandidate);
        let full_commit = full_defer.command(Command::CommitCandidate);
        assert!(!full_defer.has_deferred_candidates());
        assert_eq!(partial_commit.commit, normal_commit.commit);
        assert_eq!(full_commit.commit, normal_commit.commit);
        assert_eq!(
            partial_defer.last_committed_pinyin_spans(),
            normal.last_committed_pinyin_spans()
        );
        assert_eq!(
            full_defer.last_committed_pinyin_spans(),
            normal.last_committed_pinyin_spans()
        );
    }
}

#[cfg(unix)]
fn count_open_fds_to_path(target: &Path) -> usize {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata(target) else {
        return 0;
    };
    let (target_dev, target_ino) = (meta.dev(), meta.ino());
    let Ok(entries) = std::fs::read_dir("/dev/fd") else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            std::fs::metadata(entry.path())
                .is_ok_and(|m| m.dev() == target_dev && m.ino() == target_ino)
        })
        .count()
}

#[test]
fn read_only_bundle_overlay_json_and_one_shot_journal_zero_fds() {
    let temp = tempfile::tempdir().unwrap();
    let bundle_dir = temp.path().join("bundle");
    let user_dir = temp.path().join("user");
    std::fs::create_dir_all(&bundle_dir).unwrap();
    std::fs::create_dir_all(&user_dir).unwrap();

    let pinyin_db = bundle_dir.join("msime-pinyin.db");
    {
        let conn = rusqlite::Connection::open(&pinyin_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE pinyin_c (key TEXT, jianpin TEXT, value TEXT, weight INTEGER);
             INSERT INTO pinyin_c VALUES ('ce''shi', 'cs', '测试', 5000);
             INSERT INTO pinyin_c VALUES ('ce''shi', 'cs', '侧室', 4000);
             CREATE TABLE quick_parases (key TEXT, value TEXT, weight INTEGER);
             INSERT INTO quick_parases VALUES ('rq', '2026-01-01', 100);",
        )
        .unwrap();
    }
    let english_db = bundle_dir.join("msime-english.db");
    {
        let conn = rusqlite::Connection::open(&english_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE english_words (word TEXT, display TEXT, weight INTEGER);
             INSERT INTO english_words VALUES ('responsay', 'ResponSayBase', 1000);
             INSERT INTO english_words VALUES ('obsolete', 'obsolete', 900);",
        )
        .unwrap();
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pinyin_db, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::fs::set_permissions(&english_db, std::fs::Permissions::from_mode(0o444)).unwrap();
    }

    let user_db = user_dir.join("msime_user.db");
    {
        let conn = rusqlite::Connection::open(&user_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE user_dictionary_operations (
                dictionary TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                operation TEXT NOT NULL,
                weight INTEGER NOT NULL DEFAULT 0,
                display TEXT NOT NULL DEFAULT '',
                user_inserted INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO user_dictionary_operations VALUES ('pinyin', 'ce''shi', '策士', 'upsert', 99000, '', 1, 1);
             INSERT INTO user_dictionary_operations VALUES ('pinyin', 'ce''shi', '自动学词勿载', 'upsert', 999999, '', 0, 2);
             INSERT INTO user_dictionary_operations VALUES ('pinyin', 'ce''shi', '侧室', 'delete', 0, '', 1, 3);
             INSERT INTO user_dictionary_operations VALUES ('english', 'responsay', 'responsay', 'upsert', 5000000, 'ResponSayPro', 1, 4);
             INSERT INTO user_dictionary_operations VALUES ('english', 'obsolete', 'obsolete', 'delete', 0, 'obsolete', 1, 5);",
        )
        .unwrap();
    }

    let mut session = build_ios_session(&bundle_dir, &user_dir, 0);
    session.set_english_mixed_candidates(true);

    #[cfg(unix)]
    assert_eq!(
        count_open_fds_to_path(&user_db),
        0,
        "msime_user.db must have zero open fds after Session::new"
    );

    for b in b"ceshi" {
        session.character(*b, false);
    }
    let words: Vec<String> = session
        .snapshot()
        .candidates
        .iter()
        .map(|c| c.word.to_string())
        .collect();
    assert_eq!(words.first().map(String::as_str), Some("策士"));
    assert!(!words.iter().any(|w| w == "自动学词勿载"));
    assert!(!words.iter().any(|w| w == "侧室"));

    #[cfg(unix)]
    assert_eq!(
        count_open_fds_to_path(&user_db),
        0,
        "msime_user.db must have zero open fds during typing"
    );

    assert!(session.set_user_overlay_json("{bad json").is_err());

    session
        .set_user_overlay_json(
            r#"{"version":1,"revision":"r2","entries":[
                {"dictionary":"pinyin","key":"ce'shi","value":"新词置顶","operation":"upsert","weight":250000,"display":""},
                {"dictionary":"pinyin","key":"ce'shi","value":"测试","operation":"delete","weight":0,"display":""},
                {"dictionary":"english","key":"responsay","value":"responsay","operation":"upsert","weight":9000000,"display":"ResponSayLive"},
                {"dictionary":"english","key":"responsay","value":"responsay","operation":"delete","weight":0,"display":"ResponSayBase"}
            ]}"#,
        )
        .unwrap();

    let updated_words: Vec<String> = session
        .snapshot()
        .candidates
        .iter()
        .map(|c| c.word.to_string())
        .collect();
    assert_eq!(updated_words.first().map(String::as_str), Some("新词置顶"));
    assert!(!updated_words.iter().any(|w| w == "测试"));
    assert!(!updated_words.iter().any(|w| w == "策士"));
    session.command(Command::Cancel);

    session.set_dedicated_english(true);
    for b in b"responsay" {
        session.character(*b, false);
    }
    let eng_words: Vec<String> = session
        .snapshot()
        .candidates
        .iter()
        .map(|c| c.word.to_string())
        .collect();
    assert_eq!(eng_words.first().map(String::as_str), Some("ResponSayLive"));
    assert!(!eng_words.iter().any(|w| w == "ResponSayBase"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&pinyin_db, std::fs::Permissions::from_mode(0o644));
        let _ = std::fs::set_permissions(&english_db, std::fs::Permissions::from_mode(0o644));
    }
}
