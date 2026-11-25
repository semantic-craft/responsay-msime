// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL was not distributed with this file, You can obtain one at http://mozilla.org/MPL/2.0/.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use msime_engine::{
    autocorrect_type, Clock, Command, FrequencyAdjustmentMode, FrequencyAdjustmentOptions,
    LocalDateTime, RuntimePaths, SchemeSet, SchemeType, Session, SessionOptions,
    ShuangpinProfileKind,
};

fn lexicon_dir() -> Option<PathBuf> {
    if let Some(env_dir) = std::env::var_os("MSIME_TEST_LEXICON_DIR") {
        let dir = PathBuf::from(env_dir);
        if dir.join("msime-pinyin.db").is_file() {
            return Some(dir);
        }
    }
    let dir = PathBuf::from("/Users/xianweizhang/Projects/responsay-ios/build/Lexicon");
    dir.join("msime-pinyin.db").is_file().then_some(dir)
}

fn build_session(lexicon: &Path, temp: &Path) -> Session {
    let paths = RuntimePaths {
        resources: lexicon.to_path_buf(),
        user_data: temp.to_path_buf(),
        cache: temp.to_path_buf(),
        dictionaries: lexicon.to_path_buf(),
    };
    let mut options = SessionOptions::new(paths);
    options.enabled_schemes = SchemeSet::of(&[SchemeType::Quanpin, SchemeType::Shuangpin]);
    options.shuangpin_profile = ShuangpinProfileKind::Xiaohe;
    options.learning = false;
    options.personal_context = false;
    options.sentence_alternatives = true;
    options.mixed_candidates = true;
    options.english.mixed_candidates = true;
    options.local_modes.date_time_inline = true;
    options.autocorrect_types = autocorrect_type::TRANSPOSITION
        | autocorrect_type::NEIGHBOR
        | autocorrect_type::MISSING_OR_EXTRA;
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

fn top_candidates(session: &mut Session, input: &str, count: usize) -> Vec<String> {
    for byte in input.bytes() {
        session.character(byte, false);
    }
    let words = session
        .snapshot()
        .candidates
        .iter()
        .take(count)
        .map(|item| item.word.to_string())
        .collect();
    session.command(Command::Cancel);
    words
}

#[test]
fn real_lexicon_four_pillars_parity() {
    let Some(lexicon) = lexicon_dir() else {
        return;
    };
    let temp = tempfile::tempdir().expect("tempdir");
    let mut session = build_session(&lexicon, temp.path());

    // 1. 句中无缝中英混输（全拼 14 条基准 + 英文词典长词回退 + 上屏验证）
    let mixed_cases = [
        ("woyongiphone", "我用iPhone"),
        ("zhegeshiiosxitong", "这个是iOS系统"),
        ("qingdakaixcode", "请打开Xcode"),
        ("womenyongswiftkaifa", "我们用Swift开发"),
        ("tazaigithubshang", "他在GitHub上"),
        ("lianjiewifi", "连接WiFi"),
        ("fasongpdfwenjian", "发送PDF文件"),
        ("diaoyongapi", "调用API"),
        ("bushudaodocker", "部署到Docker"),
        ("xieyigepythonjiaoben", "写一个Python脚本"),
        ("zhegebugxiufule", "这个bug修复了"),
        ("yongmacbookbangong", "用MacBook办公"),
        ("bangwokanyixiapr", "帮我看一下PR"),
        ("kanxiaui", "看下UI"),
        ("dakaigoogle", "打开Google"),
    ];
    for (input, expected) in mixed_cases {
        let words = top_candidates(&mut session, input, 5);
        assert_eq!(
            words.first().map(String::as_str),
            Some(expected),
            "{input} 首选不符，实际前五: {words:?}"
        );
    }

    for byte in b"woyongiphone" {
        session.character(*byte, false);
    }
    let commit = session.command(Command::CommitCandidate);
    assert_eq!(commit.commit.as_deref(), Some("我用iPhone"));
    assert!(session.snapshot().preedit.is_empty());

    // 纯中文词条零回归守护：首选及无英文污染
    for (input, expected_in_top4) in [
        ("pingguo", "苹果"),
        ("shui", "水"),
        ("ai", "爱"),
        ("kanxia", "看下"),
        ("yixia", "一下"),
        ("kaifa", "开发"),
        ("woaini", "我爱你"),
        ("nihao", "你好"),
        ("zhongguo", "中国"),
    ] {
        let words = top_candidates(&mut session, input, 5);
        assert!(
            words.iter().take(4).any(|word| word == expected_in_top4),
            "纯中文 {input} 前四缺少 {expected_in_top4}，实际前五: {words:?}"
        );
        if !matches!(input, "shui" | "ai") {
            assert_eq!(
                words.first().map(String::as_str),
                Some(expected_in_top4),
                "纯中文 {input} 首选回归，实际前五: {words:?}"
            );
        }
        assert!(
            !words[0].bytes().any(|b| b.is_ascii_alphabetic()),
            "纯中文 {input} 首选混入英文: {words:?}"
        );
    }

    // 2. 候选条多样性：`jintianxiawu` 首选「今天下午」，前三位露出主前缀「今天」，不被未请求长词「今天下午考」霸占
    let jtxw = top_candidates(&mut session, "jintianxiawu", 6);
    assert_eq!(
        jtxw.first().map(String::as_str),
        Some("今天下午"),
        "{jtxw:?}"
    );
    assert!(
        jtxw.iter().take(3).any(|word| word == "今天"),
        "前三位缺少主前缀词「今天」: {jtxw:?}"
    );
    assert!(
        !jtxw
            .iter()
            .take(3)
            .any(|word| word == "今天下午考" || word == "今天下午考完"),
        "未请求长词补全仍挤占前三位: {jtxw:?}"
    );

    // 3. 句尾换位、句中邻键滑音与口语同音整句消歧
    for (input, expected) in [
        ("meiyouwenit", "没有问题"),
        ("mingtianshagnwu", "明天上午"),
        ("kaigazhegongju", "开发者工具"),
        ("zhehexiangmu", "这个项目"),
        ("gageweizhi", "发个位置"),
        ("mingtianzaoshangjiudiankaihui", "明天早上九点开会"),
        ("zhegefangankeyizhijieluodi", "这个方案可以直接落地"),
        ("ruguoyouwentiqingsuishilianxiwo", "如果有问题请随时联系我"),
        (
            "zuijintianqibianlenglezhuyibaonuan",
            "最近天气变冷了注意保暖",
        ),
        ("zaijiudiankaihui", "在酒店开会"),
        ("zhehewomenyouguan", "这和我们有关"),
    ] {
        let words = top_candidates(&mut session, input, 5);
        assert_eq!(
            words.first().map(String::as_str),
            Some(expected),
            "{input} 首选不符，实际前五: {words:?}"
        );
    }

    // 4. 行内 `xq` 星期触发（与 `rq`、`sj` 一致在首屏直接露出）
    let xq = top_candidates(&mut session, "xq", 6);
    assert!(
        xq.iter().take(5).any(|word| word == "星期四"),
        "xq 首屏缺少「星期四」: {xq:?}"
    );
    assert!(
        xq.iter().take(5).any(|word| word == "周四"),
        "xq 首屏缺少「周四」: {xq:?}"
    );
    let rq = top_candidates(&mut session, "rq", 6);
    assert!(
        rq.iter().take(6).any(|word| word == "2026年11月26日"),
        "rq 首屏缺少日期候选: {rq:?}"
    );
    let sj = top_candidates(&mut session, "sj", 6);
    assert!(
        sj.iter().take(5).any(|word| word == "10:30"),
        "sj 首屏缺少时间候选: {sj:?}"
    );

    // 双拼（小鹤）句中无缝中英混输与整句上屏验证
    session
        .switch_scheme(SchemeType::Shuangpin)
        .expect("switch shuangpin");
    for (input, expected) in [
        ("woysiphone", "我用iPhone"),
        ("fasspdfwfjm", "发送PDF文件"),
        ("woysios", "我用iOS"),
    ] {
        let words = top_candidates(&mut session, input, 5);
        assert_eq!(
            words.first().map(String::as_str),
            Some(expected),
            "双拼 {input} 首选不符，实际前五: {words:?}"
        );
    }
    for byte in b"woysiphone" {
        session.character(*byte, false);
    }
    let sp_commit = session.command(Command::CommitCandidate);
    assert_eq!(sp_commit.commit.as_deref(), Some("我用iPhone"));
    assert!(session.snapshot().preedit.is_empty());
}

#[test]
fn hermetic_four_pillars_integration() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dict_dir = temp.path().join("dict");
    let user_dir = temp.path().join("user");
    std::fs::create_dir_all(&dict_dir).expect("dict dir");
    std::fs::create_dir_all(&user_dir).expect("user dir");

    let pinyin_db = dict_dir.join("msime-pinyin.db");
    let conn = rusqlite::Connection::open(&pinyin_db).expect("open pinyin db");
    conn.execute_batch(
        "PRAGMA user_version = 2;
         CREATE TABLE tbl_1_j(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_1_j VALUES('jin','j','今',500000);
         CREATE TABLE tbl_1_m(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_1_m VALUES('mei','m','没',800000);
         CREATE TABLE tbl_1_y(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_1_y VALUES('you','y','有',900000);
         CREATE TABLE tbl_1_w(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_1_w VALUES('wen','w','问',500000);
         CREATE TABLE tbl_1_t(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_1_t VALUES('ti','t','题',500000);
         CREATE TABLE tbl_2_w(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_2_w VALUES('wo''yong','wy','我用',250000);
         INSERT INTO tbl_2_w VALUES('wen''ti','wt','问题',900000);
         CREATE TABLE tbl_2_m(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_2_m VALUES('mei''you','my','没有',1200000);
         CREATE TABLE tbl_2_j(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_2_j VALUES('jin''tian','jt','今天',1700000);
         CREATE TABLE tbl_2_g(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_2_g VALUES('gong''ju','gj','工具',400000);
         CREATE TABLE tbl_3_k(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_3_k VALUES('kai''fa''zhe','kfz','开发者',300000);
         CREATE TABLE tbl_1_k(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_1_k VALUES('kai','k','开',600000);
         CREATE TABLE tbl_1_g(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_1_g VALUES('ga','g','尬',5000);
         CREATE TABLE tbl_1_z(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_1_z VALUES('zhe','z','着',500000);
         CREATE TABLE tbl_4_j(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_4_j VALUES('jin''tian''xia''wu','jtxw','今天下午',20000);
         CREATE TABLE tbl_5_j(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_5_j VALUES('jin''tian''xia''wu''kao','jtxwk','今天下午考',100);
         CREATE TABLE tbl_2_x(key TEXT,jp TEXT,value TEXT,weight INTEGER);
         INSERT INTO tbl_2_x VALUES('xu''qiu','xq','需求',900000);
         INSERT INTO tbl_2_x VALUES('xia''qu','xq','下去',800000);
         INSERT INTO tbl_2_x VALUES('xing''qu','xq','兴趣',700000);
         INSERT INTO tbl_2_x VALUES('xin''qing','xq','心情',600000);
         INSERT INTO tbl_2_x VALUES('xiao''qu','xq','小区',500000);
         INSERT INTO tbl_2_x VALUES('xing''qi','xq','星期',100000);",
    )
    .expect("seed pinyin db");
    drop(conn);

    let mut session = build_session(&dict_dir, &user_dir);
    assert_eq!(
        top_candidates(&mut session, "woyongiphone", 3)
            .first()
            .map(String::as_str),
        Some("我用iPhone")
    );
    let jtxw = top_candidates(&mut session, "jintianxiawu", 4);
    assert_eq!(jtxw.first().map(String::as_str), Some("今天下午"));
    assert_eq!(jtxw.get(1).map(String::as_str), Some("今天"));
    for b in b"meiyouwenit" {
        session.character(*b, false);
    }
    let snap = session.snapshot();
    let mywt: Vec<String> = snap
        .candidates
        .iter()
        .take(5)
        .map(|c| c.word.to_string())
        .collect();
    let pre = snap.preedit.clone();
    session.command(Command::Cancel);
    assert_eq!(
        mywt.first().map(String::as_str),
        Some("没有问题"),
        "hermetic meiyouwenit preedit={pre:?} cands={mywt:?}"
    );
    assert_eq!(
        top_candidates(&mut session, "kaigazhegongju", 3)
            .first()
            .map(String::as_str),
        Some("开发者工具")
    );
    let xq = top_candidates(&mut session, "xq", 5);
    assert!(xq.iter().any(|w| w == "星期四"), "{xq:?}");
    assert!(xq.iter().any(|w| w == "周四"), "{xq:?}");
}
