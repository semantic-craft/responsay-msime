//! Quanpin key handling and request building (`R/schemes/quanpin_scheme.cpp`, quanpin.md §3.5).

use std::sync::Arc;

use crate::pinyin::active_helpcode::{detect_active_helpcode_length, strip_active_helpcodes};
use crate::pinyin::segment::{cut_pinyin_by_mode, join_segments, CutMode};
use crate::shuangpin::query::{apply_segmentation_cases_into, remove_manual_delimiters};
use crate::touch::{realign_touches, KeyGeometry, KeyTouch};
use crate::types::{QueryRequest, SchemeKey, SchemeType};

fn replace_string(target: &mut String, source: String) {
    if target.capacity() == 0 {
        *target = source;
    } else {
        target.clone_from(&source);
    }
}

#[derive(Debug, Clone, Default)]
pub struct QuanpinScheme {
    raw: String,
    touches: Vec<Option<KeyTouch>>,
    pending_touch: Option<KeyTouch>,
    key_geometry: Option<Arc<KeyGeometry>>,
}

impl QuanpinScheme {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_key_geometry(&mut self, geometry: Option<Arc<KeyGeometry>>) {
        if self.key_geometry != geometry {
            self.touches.clear();
            self.pending_touch = None;
        }
        self.key_geometry = geometry;
    }

    pub fn set_pending_touch(&mut self, touch: Option<KeyTouch>) {
        self.pending_touch = touch;
    }

    fn push_touch(&mut self, touch: Option<KeyTouch>) {
        if self.touches.is_empty() {
            if touch.is_none() {
                return;
            }
            self.touches.resize(self.raw.len() - 1, None);
        }
        self.touches.push(touch);
        debug_assert_eq!(self.touches.len(), self.raw.len());
    }

    pub fn reset(&mut self) {
        self.raw.clear();
        self.touches.clear();
        self.pending_touch = None;
    }

    /// Letters keep their case; `'` is appended unless the last character already is one; Backspace pops; Escape and Return reset.
    pub fn handle_key(&mut self, key: SchemeKey) -> bool {
        let touch = self.pending_touch.take();
        let pushed = match key {
            SchemeKey::Letter(letter) if letter.is_ascii_alphabetic() => {
                self.raw.push(char::from(letter));
                touch.filter(|_| letter.is_ascii_lowercase())
            }
            SchemeKey::Apostrophe => {
                if !self.raw.ends_with('\'') {
                    self.raw.push('\'');
                    None
                } else {
                    return false;
                }
            }
            SchemeKey::Backspace => {
                if self.raw.pop().is_none() {
                    return false;
                }
                self.touches.truncate(self.raw.len());
                return true;
            }
            SchemeKey::Letter(_)
            | SchemeKey::Semicolon
            | SchemeKey::Minus
            | SchemeKey::Symbol(_)
            | SchemeKey::Requery => return false,
        };
        self.push_touch(pushed);
        true
    }

    /// The correction-mode request (QS:142-155).
    pub fn build_request(&self) -> QueryRequest {
        let mut request = QueryRequest::default();
        self.build_request_into(&mut request);
        request
    }

    /// 将全拼请求写入已有存储，避免逐键刷新重复分配按键和切分字符串。
    pub fn build_request_into(&self, request: &mut QueryRequest) {
        Self::build_request_from_raw_into(&self.raw, &self.raw, request);
        request.key_touches.clear();
        request.key_geometry = None;
        let Some(geometry) = &self.key_geometry else {
            return;
        };
        let helpcode_length = if request.enable_quanpin_helpcode {
            detect_active_helpcode_length(&request.raw_input, &request.raw_input_with_cases)
        } else {
            0
        };
        if request.valid
            && helpcode_length == 0
            && self.touches.iter().any(Option::is_some)
            && request.raw_input.len() == self.touches.len()
            && !self.raw.contains('\'')
        {
            request.key_touches.extend_from_slice(&self.touches);
            request.key_geometry = Some(Arc::clone(geometry));
        }
    }

    /// 从原文直接写入查询请求，光标前缀查询不必创建临时方案。
    pub fn build_request_from_raw_into(
        raw: &str,
        raw_with_cases: &str,
        request: &mut QueryRequest,
    ) {
        request.scheme = SchemeType::Quanpin;
        request.raw_input.clear();
        request.raw_input.extend(
            raw.bytes()
                .map(|byte| char::from(byte.to_ascii_lowercase())),
        );
        let raw_with_cases = if raw_with_cases.is_empty() {
            raw
        } else {
            raw_with_cases
        };
        request.raw_input_with_cases.clear();
        request.raw_input_with_cases.push_str(raw_with_cases);
        Self::apply_segmentation(request, CutMode::Correction);
    }

    /// The raw letters, case kept.
    pub fn preedit(&self) -> String {
        self.raw.clone()
    }

    /// Host editing replaces the composition; `raw_with_cases` wins when non-empty.
    pub fn set_raw_input(&mut self, raw: &str, raw_with_cases: &str) {
        let source = if raw_with_cases.is_empty() {
            raw
        } else {
            raw_with_cases
        };
        realign_touches(&mut self.touches, self.raw.as_bytes(), source.as_bytes());
        self.raw.clear();
        self.raw.reserve(source.len());
        self.raw.push_str(source);
    }

    #[cfg(test)]
    pub(crate) fn touches(&self) -> &[Option<KeyTouch>] {
        &self.touches
    }

    /// Fill a request's segmentation fields from its raw input with the given cut (QS:82-139): active helpcodes stripped, cases re-applied, trailing `'` and helpcode letters re-appended.
    pub fn apply_segmentation(request: &mut QueryRequest, mode: CutMode) {
        // 关闭辅助码时，大写字母仍属于拼音原文，不能从切分中剥离。
        let helpcode_length = if request.enable_quanpin_helpcode {
            detect_active_helpcode_length(&request.raw_input, &request.raw_input_with_cases)
        } else {
            0
        };
        let normalized_source = if helpcode_length > 0 {
            strip_active_helpcodes(&request.raw_input, &request.raw_input_with_cases)
        } else {
            request.raw_input.clone()
        };

        let normalized_input = remove_manual_delimiters(&normalized_source);
        let normalized_input_is_empty = normalized_input.is_empty();
        replace_string(&mut request.normalized_input, normalized_input);
        let normalized_segmentation = if normalized_input_is_empty {
            String::new()
        } else {
            cut_pinyin_by_mode(&normalized_source, mode)
                .first()
                .map(|segments| join_segments(segments))
                .unwrap_or_default()
        };
        if normalized_segmentation.is_empty() {
            replace_string(&mut request.normalized_segmentation, normalized_source);
        } else {
            replace_string(
                &mut request.normalized_segmentation,
                normalized_segmentation,
            );
        }

        let cased = &request.raw_input_with_cases;
        let cased_source = if helpcode_length > 0 && cased.len() >= helpcode_length {
            &cased[..cased.len() - helpcode_length]
        } else {
            cased.as_str()
        };
        apply_segmentation_cases_into(
            &request.normalized_segmentation,
            cased_source,
            &mut request.raw_segmentation,
        );
        let raw_segmentation = &mut request.raw_segmentation;
        if cased_source.ends_with('\'') && !raw_segmentation.ends_with('\'') {
            raw_segmentation.push('\'');
        }
        if helpcode_length > 0 {
            raw_segmentation.push('\'');
            raw_segmentation.push_str(&cased[cased.len() - helpcode_length..]);
        }
        request
            .segmentation
            .clone_from(&request.normalized_segmentation);
        request.valid = !normalized_input_is_empty;
    }

    /// Re-cut literally (greedy); what autocorrect suppression does to a request.
    pub fn apply_literal_segmentation(request: &mut QueryRequest) {
        Self::apply_segmentation(request, CutMode::Greedy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> QuanpinScheme {
        let mut scheme = QuanpinScheme::new();
        for byte in text.bytes() {
            scheme.handle_key(if byte == b'\'' {
                SchemeKey::Apostrophe
            } else {
                SchemeKey::Letter(byte)
            });
        }
        scheme
    }

    #[test]
    fn refreshing_a_request_reuses_the_case_output_buffer() {
        let scheme = typed("nihaoAB");
        let mut request = QueryRequest {
            enable_quanpin_helpcode: true,
            ..QueryRequest::default()
        };
        scheme.build_request_into(&mut request);
        let expected = request.clone();
        let pointer = request.raw_segmentation.as_ptr();
        let ((), allocations) = crate::ime::personal_rerank::allocations::count(|| {
            scheme.build_request_into(&mut request);
        });
        assert_eq!(request, expected);
        assert_eq!(request.raw_segmentation, "ni'hao'AB");
        assert_eq!(request.raw_segmentation.as_ptr(), pointer);
        assert_eq!(allocations, 16);
    }

    #[test]
    fn keys_edit_the_raw_letters() {
        let mut scheme = typed("Ni''");
        assert_eq!(scheme.preedit(), "Ni'", "a doubled apostrophe is dropped");
        scheme.handle_key(SchemeKey::Semicolon);
        scheme.handle_key(SchemeKey::Minus);
        scheme.handle_key(SchemeKey::Requery);
        scheme.handle_key(SchemeKey::Letter(b'1'));
        scheme.handle_key(SchemeKey::Symbol(b'-'));
        scheme.handle_key(SchemeKey::Symbol(b' '));
        assert_eq!(scheme.preedit(), "Ni'");
        scheme.handle_key(SchemeKey::Backspace);
        assert_eq!(scheme.preedit(), "Ni");
        scheme.reset();
        assert_eq!(scheme.preedit(), "");
        let mut scheme = typed("ab");
        scheme.reset();
        assert_eq!(scheme.preedit(), "");
        scheme.handle_key(SchemeKey::Backspace);
        assert_eq!(scheme.preedit(), "");
    }

    #[test]
    fn host_raw_input_prefers_the_cased_form() {
        let mut scheme = QuanpinScheme::new();
        scheme.set_raw_input("nihao", "NiHao");
        assert_eq!(scheme.preedit(), "NiHao");
        scheme.set_raw_input("nihao", "");
        assert_eq!(scheme.preedit(), "nihao");
    }

    #[test]
    fn host_raw_input_reuses_existing_storage() {
        let mut scheme = QuanpinScheme::new();
        scheme.set_raw_input("abcdefghijklmnopqrstuvwxyz", "");
        let capacity = scheme.raw.capacity();

        scheme.set_raw_input("ni", "");

        assert_eq!(scheme.preedit(), "ni");
        assert!(scheme.raw.capacity() >= capacity);
    }

    #[test]
    fn request_is_cut_in_correction_mode() {
        let request = typed("nihao").build_request();
        assert_eq!(request.scheme, SchemeType::Quanpin);
        assert_eq!(request.raw_input, "nihao");
        assert_eq!(request.normalized_input, "nihao");
        assert_eq!(request.normalized_segmentation, "ni'hao");
        assert_eq!(request.segmentation, "ni'hao");
        assert_eq!(request.raw_segmentation, "ni'hao");
        assert!(request.valid);

        let aliased = typed("sahng").build_request();
        assert_eq!(aliased.segmentation, "shang", "the alias layer always runs");
        let mut literal = aliased.clone();
        QuanpinScheme::apply_literal_segmentation(&mut literal);
        assert_ne!(literal.segmentation, "shang");
    }

    #[test]
    fn trailing_apostrophe_and_case_survive_in_the_raw_segmentation() {
        let request = typed("NiHao'").build_request();
        assert_eq!(request.raw_input, "nihao'");
        assert_eq!(request.segmentation, "ni'hao");
        assert_eq!(request.raw_segmentation, "Ni'Hao'");
    }

    #[test]
    fn active_helpcode_letters_are_split_off() {
        let mut request = QueryRequest {
            enable_quanpin_helpcode: true,
            ..QueryRequest::default()
        };
        typed("nihaoAB").build_request_into(&mut request);
        assert_eq!(request.normalized_input, "nihao");
        assert_eq!(
            request.normalized_input.capacity(),
            request.normalized_input.len()
        );
        assert_eq!(request.segmentation, "ni'hao");
        assert_eq!(request.raw_segmentation, "ni'hao'AB");
    }

    #[test]
    fn disabled_helpcode_keeps_uppercase_letters_in_pinyin_segmentation() {
        let request = typed("maO").build_request();
        assert_eq!(request.raw_input, "mao");
        assert_eq!(request.normalized_input, "mao");
        assert_eq!(request.raw_segmentation, "maO");
    }

    #[test]
    fn empty_input_is_invalid() {
        let request = typed("'").build_request();
        assert!(!request.valid);
        assert!(!QuanpinScheme::new().build_request().valid);
    }

    fn test_geometry(id: u32, shift_x: f32) -> Arc<KeyGeometry> {
        let mut keys = [[0.0f32; 4]; 26];
        for (row, letters) in ["qwertyuiop", "asdfghjkl", "zxcvbnm"]
            .into_iter()
            .enumerate()
        {
            for (col, byte) in letters.bytes().enumerate() {
                keys[usize::from(byte - b'a')] = [
                    10.0 + shift_x + 10.0 * col as f32,
                    10.0 + 14.0 * row as f32,
                    10.0,
                    14.0,
                ];
            }
        }
        Arc::new(KeyGeometry::new(id, &keys, None, &[]).unwrap())
    }

    #[test]
    fn quanpin_touch_tracking_handles_case_apostrophe_realign_and_geometry_changes() {
        let mut scheme = QuanpinScheme::new();
        scheme.handle_key(SchemeKey::Letter(b'n'));
        assert!(scheme.touches().is_empty());
        assert_eq!(scheme.touches.capacity(), 0);
        scheme.reset();

        let geom1 = test_geometry(1, 0.0);
        scheme.set_key_geometry(Some(Arc::clone(&geom1)));
        let t1 = KeyTouch {
            dx: 2,
            dy: -1,
            copy: 0,
        };
        let t2 = KeyTouch {
            dx: -3,
            dy: 4,
            copy: 0,
        };
        scheme.set_pending_touch(Some(t1));
        scheme.handle_key(SchemeKey::Letter(b'j'));
        scheme.set_pending_touch(Some(t2));
        scheme.handle_key(SchemeKey::Letter(b'u'));
        scheme.handle_key(SchemeKey::Letter(b'n'));
        assert_eq!(scheme.touches(), &[Some(t1), Some(t2), None]);

        let req = scheme.build_request();
        assert_eq!(req.key_touches, vec![Some(t1), Some(t2), None]);
        assert!(req.key_geometry.is_some());

        scheme.set_key_geometry(Some(test_geometry(1, 0.0)));
        assert_eq!(scheme.touches(), &[Some(t1), Some(t2), None]);

        scheme.handle_key(SchemeKey::Backspace);
        assert_eq!(scheme.touches(), &[Some(t1), Some(t2)]);

        scheme.set_raw_input("u", "u");
        assert_eq!(scheme.touches(), &[Some(t2)]);

        scheme.set_raw_input("ju", "ju");
        assert_eq!(scheme.touches(), &[None, Some(t2)]);

        scheme.set_pending_touch(Some(t1));
        scheme.handle_key(SchemeKey::Letter(b'A'));
        assert_eq!(scheme.touches(), &[None, Some(t2), None]);
        let mut req_help = QueryRequest {
            enable_quanpin_helpcode: true,
            ..QueryRequest::default()
        };
        scheme.build_request_into(&mut req_help);
        assert!(req_help.key_touches.is_empty());
        assert!(req_help.key_geometry.is_none());

        scheme.handle_key(SchemeKey::Backspace);
        scheme.handle_key(SchemeKey::Apostrophe);
        assert_eq!(scheme.touches(), &[None, Some(t2), None]);
        let req_apos = scheme.build_request();
        assert!(req_apos.key_touches.is_empty());
        assert!(req_apos.key_geometry.is_none());

        scheme.set_key_geometry(Some(test_geometry(2, 1.0)));
        assert!(scheme.touches().is_empty());
    }
}
