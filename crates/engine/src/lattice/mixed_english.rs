// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL was not distributed with this file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! 句中无缝中英混输词网格注入（`MixedEnglishLexicon` 与 `MixedEnglishSpan`）：在存在相邻中文音节时识别高置信英文跨度，并沿同一套词频与二元/三元语言模型解码出规范大小写的 `CN+EN` / `CN+EN+CN` 整句。

use std::borrow::Cow;
use std::collections::HashSet;

use crate::dictionary::english::{takes_priority_slot, EnglishDictionary};
use crate::dictionary::pinyin::PinyinDatabase;
use crate::pinyin::autocorrect::autocorrect_cut_detail;
use crate::pinyin::segment::{
    cut_one_piece_min_segments, cut_pinyin_by_mode, is_complete_pinyin_input, CutMode,
};
use crate::pinyin::syllables::{canonical_intact_piece, is_intact, MAX_SYLLABLE_LENGTH};
use crate::shuangpin::query::is_complete_input;
use crate::shuangpin::utils::{
    convert_seg_shuangpin_to_seg_complete_pinyin, is_accepted_syllable_code,
};
use crate::shuangpin::ShuangpinProfile;
use crate::text::count_han_chars;
use crate::types::{autocorrect_type, CandidateSource, WordItem};

use super::decode::{
    colloquial_transition_bonus, edge_log_prob, function_word_bonus, LatticeOptions,
};
use super::ngram::SENTENCE_START;

const MAX_MIXED_INPUT_BYTES: usize = 64;
const MAX_ENGLISH_SPAN_BYTES: usize = 20;
const MIN_DICT_FALLBACK_BYTES: usize = 4;
const MIXED_BEAM_WIDTH: usize = 12;
const MIXED_SPAN_ROW_LIMIT: usize = 8;
const CURATED_BASE_WEIGHT: i64 = 5_000_000;
const CURATED_SPAN_BONUS: f64 = 4.0;
const ADJACENT_ENGLISH_PENALTY: f64 = 12.0;
const AMBIGUOUS_CN_PHRASE_BLOCK_WEIGHT: i64 = 50_000;

const ALL_AUTOCORRECT_MASK: u32 = autocorrect_type::TRANSPOSITION
    | autocorrect_type::NEIGHBOR
    | autocorrect_type::MISSING_OR_EXTRA;

/// 双语高频科技与日常词汇规范大小写词表（按小写 ASCII 键严格升序排列供二分查找），并对长度不少于 4 且非完整拼音串的输入回退查询 `EnglishDictionary`。
#[derive(Debug, Clone, Copy, Default)]
pub struct MixedEnglishLexicon;

/// 句中识别出的一段英文跨度边。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixedEnglishSpan {
    pub start: usize,
    pub end: usize,
    pub raw: String,
    pub canonical: String,
    pub weight: i64,
    pub curated: bool,
    pub secondary_only: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct MixedSentenceCandidate {
    pub item: WordItem,
    pub leads_first: bool,
}

struct LexiconHit {
    canonical: Cow<'static, str>,
    weight: i64,
    curated: bool,
}

impl MixedEnglishLexicon {
    /// 精选双语词表，按键的 ASCII 字典序严格排列。
    pub const CURATED: &'static [(&'static str, &'static str)] = &[
        ("ai", "AI"),
        ("air", "Air"),
        ("airpods", "AirPods"),
        ("android", "Android"),
        ("api", "API"),
        ("app", "App"),
        ("asr", "ASR"),
        ("bug", "bug"),
        ("cd", "CD"),
        ("chrome", "Chrome"),
        ("ci", "CI"),
        ("cli", "CLI"),
        ("cloud", "Cloud"),
        ("code", "Code"),
        ("cpu", "CPU"),
        ("demo", "Demo"),
        ("docker", "Docker"),
        ("email", "Email"),
        ("excel", "Excel"),
        ("git", "Git"),
        ("github", "GitHub"),
        ("gpt", "GPT"),
        ("gpu", "GPU"),
        ("gui", "GUI"),
        ("http", "HTTP"),
        ("https", "HTTPS"),
        ("ios", "iOS"),
        ("ipad", "iPad"),
        ("iphone", "iPhone"),
        ("java", "Java"),
        ("json", "JSON"),
        ("key", "Key"),
        ("kotlin", "Kotlin"),
        ("linux", "Linux"),
        ("llm", "LLM"),
        ("macbook", "MacBook"),
        ("macos", "macOS"),
        ("max", "Max"),
        ("mini", "Mini"),
        ("ok", "OK"),
        ("pdf", "PDF"),
        ("ppt", "PPT"),
        ("pr", "PR"),
        ("pro", "Pro"),
        ("python", "Python"),
        ("ram", "RAM"),
        ("rust", "Rust"),
        ("safari", "Safari"),
        ("sdk", "SDK"),
        ("sql", "SQL"),
        ("ssd", "SSD"),
        ("ssh", "SSH"),
        ("swift", "Swift"),
        ("swiftui", "SwiftUI"),
        ("test", "Test"),
        ("token", "Token"),
        ("tts", "TTS"),
        ("ui", "UI"),
        ("url", "URL"),
        ("usb", "USB"),
        ("ux", "UX"),
        ("vip", "VIP"),
        ("web", "Web"),
        ("wifi", "WiFi"),
        ("windows", "Windows"),
        ("word", "Word"),
        ("xcode", "Xcode"),
        ("yaml", "YAML"),
    ];

    pub fn lookup_curated(lower_key: &str) -> Option<&'static str> {
        Self::CURATED
            .binary_search_by_key(&lower_key, |&(key, _)| key)
            .ok()
            .map(|idx| Self::CURATED[idx].1)
    }

    fn lookup(
        &self,
        lower_key: &str,
        english_dict: Option<&EnglishDictionary>,
        allow_dict_fallback: bool,
    ) -> Option<LexiconHit> {
        if let Some(canonical) = Self::lookup_curated(lower_key) {
            return Some(LexiconHit {
                canonical: Cow::Borrowed(canonical),
                weight: CURATED_BASE_WEIGHT,
                curated: true,
            });
        }
        if !allow_dict_fallback || lower_key.len() < MIN_DICT_FALLBACK_BYTES {
            return None;
        }
        let item = english_dict?.query_exact(lower_key)?;
        if !takes_priority_slot(&item) {
            return None;
        }
        Some(LexiconHit {
            canonical: Cow::Owned(item.word.into_string()),
            weight: item.weight,
            curated: false,
        })
    }
}

#[derive(Clone)]
struct MixedEdge {
    end: usize,
    word: String,
    raw_piece: String,
    log_prob: f64,
    is_english: bool,
    secondary_only: bool,
}

#[derive(Clone)]
struct MixedHyp {
    score: f64,
    prev_pos: usize,
    prev_idx: usize,
    edge_idx: Option<usize>,
    has_chinese: bool,
    has_english: bool,
    all_english_secondary: bool,
}

pub(crate) fn decode_mixed_english_quanpin(
    raw: &str,
    database: &PinyinDatabase,
    english_dict: Option<&EnglishDictionary>,
    options: &LatticeOptions<'_>,
) -> Vec<MixedSentenceCandidate> {
    if !eligible_raw(raw) {
        return Vec::new();
    }
    let len = raw.len();
    let bytes = raw.as_bytes();
    let mut cn_syllables: Vec<Vec<(usize, String)>> = vec![Vec::new(); len];
    for start in 0..len {
        let max_step = (len - start).min(MAX_SYLLABLE_LENGTH);
        for step in 1..=max_step {
            if let Some(canon) = canonical_intact_piece(&bytes[start..start + step]) {
                cn_syllables[start].push((start + step, canon.to_owned()));
            }
        }
    }
    let literal_segments = cut_one_piece_min_segments(raw, true);
    let primary_boundaries = quanpin_primary_boundaries(raw, &literal_segments);
    let single_edit_typo =
        autocorrect_cut_detail(raw, ALL_AUTOCORRECT_MASK).is_some_and(|cut| cut.edge_count == 1);
    decode_mixed_graph(
        raw,
        &cn_syllables,
        primary_boundaries.as_deref(),
        (!literal_segments.is_empty()).then_some(literal_segments.as_slice()),
        single_edit_typo,
        database,
        english_dict,
        options,
        is_complete_pinyin_input,
        quanpin_interval_cuts,
    )
}

pub(crate) fn decode_mixed_english_shuangpin(
    raw: &str,
    profile: &ShuangpinProfile,
    database: &PinyinDatabase,
    english_dict: Option<&EnglishDictionary>,
    options: &LatticeOptions<'_>,
) -> Vec<MixedSentenceCandidate> {
    if !eligible_raw(raw) {
        return Vec::new();
    }
    let len = raw.len();
    let bytes = raw.as_bytes();
    let mut cn_syllables: Vec<Vec<(usize, String)>> = vec![Vec::new(); len];
    for start in 0..len {
        if start + 2 <= len && crate::shuangpin::utils::takes_two_keys(bytes, start, profile) {
            let slice = &raw[start..start + 2];
            let canon = convert_seg_shuangpin_to_seg_complete_pinyin(slice, profile);
            if is_intact(&canon) {
                cn_syllables[start].push((start + 2, canon));
            }
        }
    }
    let whole_even = is_complete_input(raw, profile);
    let boundaries = whole_even.then(|| (0..=len).step_by(2).collect::<Vec<usize>>());
    decode_mixed_graph(
        raw,
        &cn_syllables,
        boundaries.as_deref(),
        None,
        false,
        database,
        english_dict,
        options,
        |slice| is_complete_input(slice, profile),
        |slice| shuangpin_interval_cuts(slice, profile),
    )
}

pub(crate) fn merge_mixed_english_candidates(
    candidates: &mut Vec<WordItem>,
    mixed: Vec<MixedSentenceCandidate>,
) {
    if mixed.is_empty() {
        return;
    }
    let mut lead_insert = 0usize;
    for entry in mixed {
        if candidates.iter().any(|item| item.word == entry.item.word) {
            continue;
        }
        if entry.leads_first {
            candidates.insert(lead_insert, entry.item);
            lead_insert += 1;
        } else {
            let at = lead_insert.max(1).min(candidates.len());
            candidates.insert(at, entry.item);
        }
    }
}

fn eligible_raw(raw: &str) -> bool {
    (3..=MAX_MIXED_INPUT_BYTES).contains(&raw.len())
        && !raw.contains('\'')
        && raw.bytes().all(|byte| byte.is_ascii_lowercase())
}

fn quanpin_primary_boundaries(raw: &str, primary_segments: &[String]) -> Option<Vec<usize>> {
    if primary_segments.is_empty()
        || !primary_segments.iter().all(|seg| is_intact(seg))
        || primary_segments.iter().map(String::len).sum::<usize>() != raw.len()
    {
        return None;
    }
    let mut boundaries = Vec::with_capacity(primary_segments.len() + 1);
    let mut offset = 0usize;
    boundaries.push(0);
    for seg in primary_segments {
        if !raw[offset..].starts_with(seg.as_str()) {
            return None;
        }
        offset += seg.len();
        boundaries.push(offset);
    }
    Some(boundaries)
}

#[allow(clippy::too_many_arguments)]
fn decode_mixed_graph(
    raw: &str,
    cn_syllables: &[Vec<(usize, String)>],
    primary_boundaries: Option<&[usize]>,
    primary_segments: Option<&[String]>,
    single_edit_typo: bool,
    database: &PinyinDatabase,
    english_dict: Option<&EnglishDictionary>,
    options: &LatticeOptions<'_>,
    is_complete_chinese_slice: impl Fn(&str) -> bool,
    interval_cuts: impl Fn(&str) -> Vec<Vec<(usize, usize, String)>>,
) -> Vec<MixedSentenceCandidate> {
    let len = raw.len();
    let mut cn_prefix = vec![false; len + 1];
    cn_prefix[0] = true;
    for s in 0..len {
        if !cn_prefix[s] {
            continue;
        }
        for &(e, _) in &cn_syllables[s] {
            cn_prefix[e] = true;
        }
    }
    let whole_chinese = cn_prefix[len];

    let mut cn_suffix = vec![false; len + 1];
    cn_suffix[len] = true;
    for s in (0..len).rev() {
        if cn_syllables[s].iter().any(|&(e, _)| cn_suffix[e]) {
            cn_suffix[s] = true;
        }
    }

    let lexicon = MixedEnglishLexicon;
    let mut english_spans: Vec<MixedEnglishSpan> = Vec::new();
    for a in 0..len {
        if a > 0 && !cn_prefix[a] {
            continue;
        }
        let max_b = len.min(a + MAX_ENGLISH_SPAN_BYTES);
        for b in (a + 2)..=max_b {
            if a == 0 && b == len {
                continue;
            }
            if b < len && !cn_suffix[b] {
                continue;
            }
            let slice = &raw[a..b];
            if whole_chinese {
                if let Some(bounds) = primary_boundaries {
                    if bounds.binary_search(&a).is_err() || bounds.binary_search(&b).is_err() {
                        continue;
                    }
                } else {
                    continue;
                }
            }
            let allow_dict_fallback = !whole_chinese && !is_complete_chinese_slice(slice);
            let Some(hit) = lexicon.lookup(slice, english_dict, allow_dict_fallback) else {
                continue;
            };
            let mut secondary_only = !hit.curated && single_edit_typo;
            if whole_chinese {
                if let (Some(bounds), Some(segs)) = (primary_boundaries, primary_segments) {
                    let Ok(sa) = bounds.binary_search(&a) else {
                        continue;
                    };
                    let Ok(sb) = bounds.binary_search(&b) else {
                        continue;
                    };
                    let syl_span = &segs[sa..sb];
                    if syl_span.len() <= 1 {
                        secondary_only = true;
                    } else {
                        let strong_cn_word = database
                            .query_lattice_span(syl_span, 1)
                            .first()
                            .is_some_and(|row| {
                                count_han_chars(&row.value) == syl_span.len()
                                    && row.weight >= AMBIGUOUS_CN_PHRASE_BLOCK_WEIGHT
                            });
                        if strong_cn_word {
                            continue;
                        }
                    }
                } else {
                    // 双拼整串已经是完整音节时，两键英文与单音节同长，只放次选，不抢纯中文首选。
                    if b - a <= 2 {
                        secondary_only = true;
                    }
                }
            }
            english_spans.push(MixedEnglishSpan {
                start: a,
                end: b,
                raw: slice.to_owned(),
                canonical: hit.canonical.into_owned(),
                weight: hit.weight,
                curated: hit.curated,
                secondary_only,
            });
        }
    }

    if english_spans.is_empty() {
        return Vec::new();
    }

    // 计算可到达终点 `len` 的字节位置（中文单音节边 + 英文跨度边），只为有效路径查中文词库。
    let mut can_reach_end = vec![false; len + 1];
    can_reach_end[len] = true;
    for pos in (0..len).rev() {
        let via_cn = cn_syllables[pos].iter().any(|&(e, _)| can_reach_end[e]);
        let via_en = english_spans
            .iter()
            .any(|span| span.start == pos && can_reach_end[span.end]);
        can_reach_end[pos] = via_cn || via_en;
    }
    if !can_reach_end[0] {
        return Vec::new();
    }

    let mut can_reach_from_start = vec![false; len + 1];
    can_reach_from_start[0] = true;
    for pos in 0..len {
        if !can_reach_from_start[pos] {
            continue;
        }
        for &(e, _) in &cn_syllables[pos] {
            can_reach_from_start[e] = true;
        }
        for span in english_spans.iter().filter(|span| span.start == pos) {
            can_reach_from_start[span.end] = true;
        }
    }

    let max_phrase_syl = options.max_phrase_syllables.clamp(1, 8);
    let span_limit = options.span_limit.clamp(1, MIXED_SPAN_ROW_LIMIT);
    let mut outgoing: Vec<Vec<MixedEdge>> = vec![Vec::new(); len];

    let mut left_anchors: Vec<usize> = vec![0];
    let mut right_anchors: Vec<usize> = vec![len];
    for span in &english_spans {
        if !can_reach_from_start[span.start] || !can_reach_end[span.end] {
            continue;
        }
        if !left_anchors.contains(&span.end) && span.end < len {
            left_anchors.push(span.end);
        }
        if !right_anchors.contains(&span.start) && span.start > 0 {
            right_anchors.push(span.start);
        }
        let equiv_syllables = (span.end - span.start).div_ceil(2).max(1);
        let base_weight = if span.curated {
            CURATED_BASE_WEIGHT.max(span.weight)
        } else {
            span.weight
        };
        let log_prob = (base_weight as f64).ln()
            + options.phrase_length_bonus * (equiv_syllables.saturating_sub(1) as f64)
            + if span.curated {
                CURATED_SPAN_BONUS
            } else {
                0.0
            };
        outgoing[span.start].push(MixedEdge {
            end: span.end,
            word: span.canonical.clone(),
            raw_piece: span.raw.clone(),
            log_prob,
            is_english: true,
            secondary_only: span.secondary_only,
        });
    }

    let mut visited_spans: HashSet<(usize, usize, String)> = HashSet::new();
    for &cs in &left_anchors {
        if !can_reach_from_start[cs] {
            continue;
        }
        for &ce in &right_anchors {
            if ce <= cs || !can_reach_end[ce] {
                continue;
            }
            for cut in interval_cuts(&raw[cs..ce]) {
                for i in 0..cut.len() {
                    let abs_start = cs + cut[i].0;
                    let max_j = cut.len().min(i + max_phrase_syl);
                    for j in (i + 1)..=max_j {
                        let abs_end = cs + cut[j - 1].1;
                        let syl_vec: Vec<String> =
                            cut[i..j].iter().map(|(_, _, syl)| syl.clone()).collect();
                        let joined_key = syl_vec.join("'");
                        if !visited_spans.insert((abs_start, abs_end, joined_key)) {
                            continue;
                        }
                        let raw_piece = cut[i..j]
                            .iter()
                            .map(|&(rs, re, _)| &raw[cs + rs..cs + re])
                            .collect::<Vec<_>>()
                            .join("'");
                        let rows = database.query_lattice_span(&syl_vec, span_limit);
                        let syl_len = syl_vec.len();
                        for lexeme in rows {
                            if !options.offensive.admits_edge(
                                &lexeme.value,
                                abs_start,
                                abs_end,
                                len,
                            ) {
                                continue;
                            }
                            let log_prob = edge_log_prob(lexeme.weight, syl_len, options)
                                + function_word_bonus(&lexeme.value, &syl_vec);
                            outgoing[abs_start].push(MixedEdge {
                                end: abs_end,
                                word: lexeme.value,
                                raw_piece: raw_piece.clone(),
                                log_prob,
                                is_english: false,
                                secondary_only: false,
                            });
                        }
                    }
                }
            }
        }
    }

    let mut columns: Vec<Vec<MixedHyp>> = vec![Vec::new(); len + 1];
    columns[0].push(MixedHyp {
        score: 0.0,
        prev_pos: 0,
        prev_idx: 0,
        edge_idx: None,
        has_chinese: false,
        has_english: false,
        all_english_secondary: true,
    });

    let bigram = options.bigram.as_deref();
    for pos in 0..len {
        if columns[pos].is_empty() {
            continue;
        }
        columns[pos].sort_by(|a, b| b.score.total_cmp(&a.score));
        columns[pos].truncate(MIXED_BEAM_WIDTH);
        let current_hyps = columns[pos].clone();
        for (hyp_idx, hyp) in current_hyps.iter().enumerate() {
            let prev_edge = hyp.edge_idx.map(|eidx| &outgoing[hyp.prev_pos][eidx]);
            let prev_word = prev_edge.map_or(SENTENCE_START, |edge| edge.word.as_str());
            let prev_is_english = prev_edge.is_some_and(|edge| edge.is_english);
            for (edge_idx, edge) in outgoing[pos].iter().enumerate() {
                let mut score = hyp.score + edge.log_prob;
                if prev_is_english && edge.is_english {
                    score -= ADJACENT_ENGLISH_PENALTY;
                } else if !prev_is_english && !edge.is_english {
                    let static_bonus = bigram
                        .map_or(0.0, |table| f64::from(table.bigram(prev_word, &edge.word)))
                        .max(0.0);
                    score += options.bigram_weight * static_bonus;
                    score += colloquial_transition_bonus(prev_word, &edge.word, static_bonus);
                } else {
                    score += bilingual_collocation_bonus(
                        prev_word,
                        &edge.word,
                        prev_is_english,
                        edge.is_english,
                    );
                }
                columns[edge.end].push(MixedHyp {
                    score,
                    prev_pos: pos,
                    prev_idx: hyp_idx,
                    edge_idx: Some(edge_idx),
                    has_chinese: hyp.has_chinese || !edge.is_english,
                    has_english: hyp.has_english || edge.is_english,
                    all_english_secondary: hyp.all_english_secondary
                        && (!edge.is_english || edge.secondary_only),
                });
            }
        }
    }

    let mut terminal: Vec<MixedHyp> = columns[len]
        .iter()
        .filter(|hyp| hyp.has_chinese && hyp.has_english)
        .cloned()
        .collect();
    if terminal.is_empty() {
        return Vec::new();
    }
    terminal.sort_by(|a, b| b.score.total_cmp(&a.score));
    terminal.truncate(MIXED_BEAM_WIDTH);

    let trigram = options.trigram.as_deref();
    let mut results: Vec<(f64, MixedSentenceCandidate)> = Vec::new();
    for hyp in &terminal {
        let mut trace: Vec<&MixedEdge> = Vec::new();
        let mut cur_hyp = hyp;
        while let Some(edge_idx) = cur_hyp.edge_idx {
            let prev_pos = cur_hyp.prev_pos;
            let edge = &outgoing[prev_pos][edge_idx];
            trace.push(edge);
            cur_hyp = &columns[prev_pos][cur_hyp.prev_idx];
            if prev_pos == 0 && cur_hyp.edge_idx.is_none() {
                break;
            }
        }
        trace.reverse();
        let mut sentence = String::new();
        let mut typed_pinyin = String::new();
        let mut words = Vec::with_capacity(trace.len());
        for (idx, edge) in trace.iter().enumerate() {
            if idx > 0 {
                typed_pinyin.push('\'');
            }
            typed_pinyin.push_str(&edge.raw_piece);
            sentence.push_str(&edge.word);
            words.push(edge.word.clone());
        }
        if results.iter().any(|(_, cand)| cand.item.word == sentence) {
            continue;
        }
        let mut total_score = hyp.score;
        if let Some(trigram) = trigram {
            if options.trigram_weight != 0.0 {
                for idx in 0..trace.len() {
                    if trace[idx].is_english {
                        continue;
                    }
                    let prev_ok = idx == 0 || !trace[idx - 1].is_english;
                    if !prev_ok {
                        continue;
                    }
                    let before = if idx >= 2 && !trace[idx - 2].is_english {
                        trace[idx - 2].word.as_str()
                    } else {
                        SENTENCE_START
                    };
                    let previous = if idx >= 1 {
                        trace[idx - 1].word.as_str()
                    } else {
                        SENTENCE_START
                    };
                    total_score += options.trigram_weight
                        * f64::from(trigram.trigram(before, previous, &trace[idx].word));
                }
            }
        }
        let mut item = WordItem::new(
            typed_pinyin,
            sentence,
            (total_score * 1000.0) as i64,
            CandidateSource::Generated,
            "",
        );
        item.sentence_association = true;
        item.sentence_words = words;
        results.push((
            total_score,
            MixedSentenceCandidate {
                item,
                leads_first: !hyp.all_english_secondary,
            },
        ));
    }

    results.sort_by(|a, b| b.0.total_cmp(&a.0));
    results.truncate(2);
    results.into_iter().map(|(_, cand)| cand).collect()
}

fn quanpin_interval_cuts(slice: &str) -> Vec<Vec<(usize, usize, String)>> {
    let primary = cut_one_piece_min_segments(slice, true);
    if primary.is_empty() {
        return Vec::new();
    }
    let mut cuts = Vec::new();
    if let Some(mapped) = map_literal_cut_offsets(slice, &primary) {
        cuts.push(mapped);
    }
    for alt in cut_pinyin_by_mode(slice, CutMode::Correction) {
        if alt.len() == primary.len() {
            if let Some(mapped) = map_literal_cut_offsets(slice, &alt) {
                if !cuts.contains(&mapped) {
                    cuts.push(mapped);
                }
            }
        }
    }
    cuts
}

fn map_literal_cut_offsets(
    slice: &str,
    segments: &[String],
) -> Option<Vec<(usize, usize, String)>> {
    let mut out = Vec::with_capacity(segments.len());
    let mut offset = 0usize;
    for seg in segments {
        if !slice[offset..].starts_with(seg.as_str()) {
            return None;
        }
        let canon = canonical_intact_piece(seg.as_bytes())?;
        out.push((offset, offset + seg.len(), canon.to_owned()));
        offset += seg.len();
    }
    (offset == slice.len()).then_some(out)
}

fn shuangpin_interval_cuts(
    slice: &str,
    profile: &ShuangpinProfile,
) -> Vec<Vec<(usize, usize, String)>> {
    if !is_complete_input(slice, profile) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut pos = 0usize;
    let bytes = slice.as_bytes();
    while pos < bytes.len() {
        let step = if crate::shuangpin::utils::takes_two_keys(bytes, pos, profile) {
            2
        } else {
            1
        };
        if pos + step > bytes.len() {
            return Vec::new();
        }
        let sub = &slice[pos..pos + step];
        if !is_accepted_syllable_code(sub, profile) {
            return Vec::new();
        }
        let canon = convert_seg_shuangpin_to_seg_complete_pinyin(sub, profile);
        if !is_intact(&canon) {
            return Vec::new();
        }
        out.push((pos, pos + step, canon));
        pos += step;
    }
    vec![out]
}

fn bilingual_collocation_bonus(
    prev_word: &str,
    curr_word: &str,
    prev_is_english: bool,
    curr_is_english: bool,
) -> f64 {
    if !prev_is_english && curr_is_english {
        if prev_word == "连接" && matches!(curr_word, "WiFi" | "SSH" | "USB") {
            return 0.6;
        }
        if matches!(
            prev_word,
            "打开"
                | "请打开"
                | "使用"
                | "我用"
                | "用"
                | "调用"
                | "部署到"
                | "发送"
                | "看下"
                | "看一下"
        ) {
            return 0.3;
        }
    } else if prev_is_english
        && !curr_is_english
        && matches!(curr_word, "上" | "里" | "中" | "下" | "的" | "了")
    {
        return 1.0;
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curated_lexicon_is_strictly_sorted_and_complete() {
        for pair in MixedEnglishLexicon::CURATED.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "CURATED 未严格升序: {:?} >= {:?}",
                pair[0],
                pair[1]
            );
        }
        for &(key, expected) in MixedEnglishLexicon::CURATED {
            assert_eq!(MixedEnglishLexicon::lookup_curated(key), Some(expected));
        }
        assert_eq!(
            MixedEnglishLexicon::lookup_curated("iphone"),
            Some("iPhone")
        );
        assert_eq!(
            MixedEnglishLexicon::lookup_curated("github"),
            Some("GitHub")
        );
        assert_eq!(MixedEnglishLexicon::lookup_curated("wifi"), Some("WiFi"));
        assert_eq!(MixedEnglishLexicon::lookup_curated("xcode"), Some("Xcode"));
        assert_eq!(MixedEnglishLexicon::lookup_curated("macos"), Some("macOS"));
        assert_eq!(
            MixedEnglishLexicon::lookup_curated("macbook"),
            Some("MacBook")
        );
        assert_eq!(MixedEnglishLexicon::lookup_curated("nonexistent"), None);
    }
}
