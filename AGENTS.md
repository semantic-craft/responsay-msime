# AGENTS.md

给在本仓库工作的编码代理的操作约定。**架构边界、验证流程与各平台覆盖面见 [ARCHITECTURE.md](docs/ARCHITECTURE.md)，贡献流程见 [CONTRIBUTING.md](.github/CONTRIBUTING.md)；先读这两份，本文只写它们未覆盖的操作细节。**

## 改动范围

- 只暂存明确列出的路径，禁 `git add -A`。不复制相邻仓库未提交内容；上游以远端默认分支与固定提交为准。日志、测试和提交禁含真实输入、凭据或隐私，测试数据用合成值。
- **一个 issue 一个 PR。** 每个 issue 独占分支与 PR，写 `Fixes #N`；仅当行为或平台未做全时写 `Refs #N` 并列缺口。未在真机、报告者环境或真实编辑器复验不算未做完（写进验证段，合并后请报告者确认，仍写 `Fixes`）。禁多 issue 合一 PR；顺手修的无 issue 小问题并入当前 PR。见 [2026-10-11-one-issue-one-pr.md](.agents/notes/implemented/process/2026-10-11-one-issue-one-pr.md)。
- **Issue 调查评论合并发布。** 本地集齐代码、测试、上游与笔记证据后，同轮调查只发一条总结评论（结论、证据、下一步）；已有评论优先编辑合并，不逐条刷中间命令。需报告者补充时一次列全字段，无新据不催。纯机器人转述无真人补充可按授权关闭，有真人报告或材料的必留并处理。
- **「把功能对齐某个平台」不等于「把默认值改成那个平台的出厂值」。** 对齐标准是**能力可达**（字段存在、宿主读取、设置页有控件、按选择生效）。出厂默认值改变老用户手感，须单独授权；对齐时发现默认值不同**只记入决策笔记不改**（列参考值、现值、不动原因）。任何改已有出厂值或缺字段读法的 PR 须链接维护者授权（issue/评论）；推翻一周内刚定的默认值还须链原笔记并写明要求人。见 [2026-11-02-merge-and-code-health-lessons.md](.agents/notes/implemented/process/2026-11-02-merge-and-code-health-lessons.md)。
- **提交前跑本地 quick 门禁；`develop` 红着时先修红。** 发布 workflow 仅手动触发，无发布指令禁碰。最近一次 `develop` 推送 CI（Core/Native Platform/iOS/macOS CI、macOS package）或 nightly 红时先修，修好前不合功能 PR（除非 `ci-red` issue 已确认无关）。开 PR 前查 `gh run list --branch develop --limit 10` 与 `gh issue list --label ci-red`，有红在正文写明红在哪及是否相关。见 [2026-10-28-ci-red-tracking.md](.agents/notes/implemented/process/2026-10-28-ci-red-tracking.md)。
- **连着合几个 PR 时，逐个同步新 `develop` 等绿。** 后合 PR 若与刚合 PR 同改某平台或共享代码（`crates/`、`shared/`、`packages/`），先 `gh pr update-branch <n>` 再 `gh pr checks <n> --required --watch` 转绿再合；不相干平台可直合。见 [2026-10-30-push-and-merge-sequencing.md](.agents/notes/implemented/process/2026-10-30-push-and-merge-sequencing.md)。
- **改文件读写、目录遍历、权限或安卓资源（drawable/主题/布局），合并前须过安卓模拟器。** 编译/lint/JVM 测不到 `/data` 权限、SELinux 硬链接与旧系统资源差异。本机跑 `bash platforms/android/tests/device/start-emulator.sh 28`（及 `35`），`MSIME_ANDROID_TEST_SERIAL=emulator-5566 bash platforms/android/tests/device/build-core-test.sh` 编 `client-{base,core,providers,services}` 单测，经 `run-core-test.sh` 与 `msime-engine` 词库租约单测上机跑通，再跑 `smoke.sh emulator-5566 --core`（API 35 不加 `--core`）。PR 上的 `android-device-pr` 仅在改键盘或设置界面时跑 API 35（见 [2026-10-28-android-device-on-ui-prs.md](.agents/notes/implemented/process/2026-10-28-android-device-on-ui-prs.md)），API 28 及其他改动仍靠本机。
- **PR 截图一律上传 `sugi.moe` 嵌入正文。** 改任何可见内容须在「截图」节附真实尺寸与底色（含系统裁剪效果）的改前/改后图；未改视觉写「无」。用 `sugi up <图>`（`curl -fsSL https://sugi.moe/install.sh | bash` 或本机 `bin/sugi-upload`）或 `curl -F file=@<图> https://sugi.moe/api/upload`（每 IP 10 秒限 5 次，429 等 10 秒），贴前把 `https://files.sugi.moe/<id>.<ext>` 下载回验字节一致；不可用时退回 `files.catbox.moe` 直链，禁用 `tmpfiles.org`/`file.io` 等广告站。上传公开永久，禁含真实输入、账号、隐私、本机路径或凭据；保留本机原图。见 [2026-11-02-pr-images-on-sugi.md](.agents/notes/implemented/process/2026-11-02-pr-images-on-sugi.md)。
- **不要把 Tauri 生成的工程当成某个平台的产品去启动、调试或验收。** 产品本体是 `platforms/<os>` 原生宿主（React 设置页仅用于 macOS/Linux Tauri 设置与 HarmonyOS ArkWeb rawfile，Windows 仅拉 Tauri 共享面板，iOS 与 Android 发布 APK 均不含，见 [ARCHITECTURE.md](docs/ARCHITECTURE.md)）；`apps/desktop` 目录名与 Tauri bundle id 不构成例外。
- **界面正在全部迁到原生，`packages/ui` 只修缺陷，不加新设置。** 新设置先把校验、默认值和业务规则写进共享 Rust（`client-{core,services,providers}`，经 `host-api`），再落原生设置；禁往 Tauri/React 加新页/面板/入口或新 WebView 宿主（顺序 Windows → macOS → HarmonyOS → Linux，见 [2026-11-03-all-native-ui.md](.agents/notes/proposed/architecture/2026-11-03-all-native-ui.md)）。

## 合并

`develop` 允许管理员绕过且自动合并以管理员运行，以下全靠合并方自守（详例见 [2026-11-02-merge-and-code-health-lessons.md](.agents/notes/implemented/process/2026-11-02-merge-and-code-health-lessons.md)）：

- **必过检查与 `android-device-pr` 终态成功后再合**，禁用 `gh pr merge --admin` 或先合再看；靠设备 job 补跑的 PR 在转绿前不算验证过。
- **PR 正文自设的合并条件有约束力。** 写有「等报告者确认」或验证段含「未完成/未运行」本地门禁时，条件兑现或改写说明前禁合，禁转 ready 秒合；未复现的猜测根因不进 `develop`。
- **不改已有回归测试前置条件迁就新改动。** 禁关开关、换默认值或换输入绕过红灯；确需改须在正文说明旧前提失效原因并保留出厂默认用例。
- **准备合并或开 auto-merge 后不再推提交**；合前核对 `gh pr view <n> --json headRefOid` 等于本地 `git rev-parse HEAD`（见 [2026-10-30-push-and-merge-sequencing.md](.agents/notes/implemented/process/2026-10-30-push-and-merge-sequencing.md)）。
- **验证段写「交给 CI」前确认 CI 真跑该项。** `scripts/run-checks.sh` 中归 `scripts/verify-local.sh` 的 `special_checks`（`windows-native-run`、`windows-32bit-compile`、`harmony-settings-bundle`）仅本地跑，Windows GNU 交叉编译只编不跑 `platforms/windows/tests/`；触及时本机跑 `python3 scripts/test-windows-native-run.py` 等，跑不了写「未验证」。

## 代码健康

- **新断言写成行为测试，禁向 `platforms/harmony/tests/run.sh` 和 `platforms/android/check-host.sh` 加源码 `grep`。** 写进 JVM 冒烟、`*DeviceSmoke` 或 `platforms/harmony/tests/*.test.ts`；改到旧源码守卫涉及代码时顺手换为行为测试或删掉。
- **不再往巨型文件里加功能，新功能放同目录新文件。** 触及 `platforms/windows/settings/main.cpp`、`KeyboardView.ets`、`MSIMEInputService.java`、`KeyboardViewController.swift`、`crates/engine/src/nine_key.rs` 等只留接线，主体进新文件（Windows 设置页按 `*PageModel.h` 分页）；未经单独授权不整体拆分。
- **运行时从本仓分支拉取的文件不能当信任根。** `develop` 自动合并，客户端须在编译期钉死外部资源的主机、仓库、文件名与摘要（语法模型仅认编译期固定的上游 Release，见 [2026-10-26-grammar-model-local-snapshot.md](.agents/notes/implemented/feature/2026-10-26-grammar-model-local-snapshot.md)）。

## 界面

- **重布局少解释，不牺牲易用性。** 靠层级、分组、间距、对齐、排版与清晰控件名达意，不加冗余灰字；必要标签、无障碍、输入限制、错误提示与关键确认必留。
- **设置项说明一律收进标题旁的「?」，全平台。** 六平台均不常驻灰字解释（单项进标题旁「?」，组进组标题旁「?」；状态、当前值、错误、元信息、前置条件、宿主限制、空列表提示及按钮旁后果一句留原处）。共享页分 `Row` 的 `description`（进「?」）与 `detail`（常驻），原生页用各自「?」组件，见 [ARCHITECTURE.md](docs/ARCHITECTURE.md)「设置项说明收进「?」」。
- **改界面要留图。** 保留改前/改后验收截图并在 PR 验证段列明，勿随临时文件删掉。

## 工具链

- Xcode 27 模拟器界面为 `/Applications/Xcode.app/Contents/Applications/DeviceHub.app`（`Simulator.app` 已移除，`open -a Simulator` / `open -b com.apple.iphonesimulator` 必败），`xcrun simctl` 的 `boot`/`install`/`launch`/`screenshot` 照常；状态看 `xcrun simctl list devices`，看画面才开 DeviceHub。

## 本机安装

- 维护者说「安装 / 更新安装 / 切到 develop 安装」指**整套打包安装**，禁只跑 `platforms/macos/scripts/install.sh`（会漏设置应用与 `msime-mcp` 且无 `TAURI_CONFIG` 版本覆盖，见 [2026-10-19-local-install-means-full-package.md](.agents/notes/implemented/process/2026-10-19-local-install-means-full-package.md)）。先按「Worktree」查磁盘，再用 `platforms/macos/package-release.sh`（配 `MACOS_SIGNING_IDENTITY` 与 `MSIME_SPARKLE_ROOT`，universal 双架构）打完整包，备份替换 `/Applications/MSIME.app`，启动一次装好内嵌 `水杉输入法.app`，确认两者版本均为 `platforms/macos/version.txt`。

## 语言

- 新增或修改的代码注释（Rust/Swift/C++/Kotlin/ArkTS/TS/Python/脚本）、PR 标题与正文、PR/issue 评论及回复、Code review 总结与行内评论**一律使用中文**（优先于全局英文约定）。
- 标识符、命令、路径、错误码、日志与接口字段保原样加反引号；改到旧英文注释时顺手改中文，不批量重写未触及代码。
- PR 标题保留英文 Conventional Commits 类型前缀、冒号后写中文（如 `fix(linux): 焦点进入时显示当前输入模式`，受 `pr-triage.yml` 与 `release-please` 约束）；commit message 见下节。

## 提交

- 格式为 `type(scope): 摘要`（Conventional Commits）；每个可独立验证的切片单独提交；不添加自动生成标记、AI 署名或 `Co-Authored-By` 水印。

## 决策笔记

- 非平凡改动须在 `.agents/notes/` 写简体中文笔记（结构 `{lifecycle}/{class}/yyyy-mm-dd-topic.md` 与格式见 [SKILL.md](.agents/skills/write-notes-like-deepseek/SKILL.md)，`quality.yml` 跑同款校验）；样式/格式化/打标等不改行为补丁免写。
- 动手前搜 `rg --hidden --glob '!.agents/notes/archived/**' "<关键词>" .agents/notes/`：有归属就地更新，决定翻转才新建互链。新想法写 `proposed/`，落地随代码转 `implemented/`，被否且值得记进 `rejected/`。
- 备选方案先写最强理由再写弃因；确无备选时保留 `## Alternatives considered` 标题并写「无」。提交前跑 `pnpm run verify-notes`。

## 发版

- 先把 `platforms/<os>/version.txt`（macOS 含 `build-number.txt`）经普通 PR 合入 `develop`，再从 `develop` 切 `release/<版本号>` 向 `main` 开 PR（`branch-guard.yml` 禁 `develop` 直合 `main`），用 **merge commit** 合入（禁 squash，防下次发版冲突）。
- 分支后缀：`main` 无后缀（正式版，更新 Homebrew cask 与 Linux 包定义），`develop` 加 `-beta`（测试版/每日构建通道提供，见 `crates/client-services/src/update_check.rs`、`UpdateApi.java`），其他分支加 `-alpha`（更新检查不推）；包内版本恒为 `X.Y.Z`，带后缀皆标 prerelease。
- Release Android 的 `publish` 依赖 `android-device.yml`（API 28/35 模拟器单测与装包），失败查 logcat，禁删依赖。网页引擎（`@msime/web-engine`）勾选 `publish` 仅限 `main`（防污染 npm `latest` 与无版本 CDN；只构建不限分支）。

## Worktree

- **位置与命名**：建在仓库外部 `~/worktrees/<feature>/` 或副盘 `worktrees/`（kebab-case，不加 `wt-` 前缀）；禁放仓内、主工作区旁、桌面或易被系统清理的 `/tmp`、`/var/tmp`。途中勤做 WIP 提交。
- **Windows `-RustOutputs`**：新 worktree `target/` 为空，仅改原生代码（TSF/Server/WinUI 设置）时，先在有缓存区跑 `Build-RustOutputs.ps1 -Architecture x64|x86|arm64 -OutputDirectory <目录>`，再传 `-RustOutputs <目录>` 给 `Build-Client.ps1`、`installer/test.ps1` 或 `test-light.ps1` 免重编近 400 crate；改了 Rust 则 Server 与 TSF 必须同源重编，禁混用。
- **推送前查 PR 状态**：合并后远端自动删分支，续推会重建死分支且提交静默丢在 `develop` 外。每次推送前跑 `gh pr list --head <branch> --state all --json number,state`；已 `MERGED` 则从 `origin/develop` 新切分支 cherry-pick 另开 PR；关而不合的 PR 顺手 `git push origin --delete <branch>`。见 [2026-10-30-push-and-merge-sequencing.md](.agents/notes/implemented/process/2026-10-30-push-and-merge-sequencing.md)。
- **固定产物目录与磁盘水位（`>= 50 GB`）**：只用规定的 `target/`、`target/<platform>-cargo` 及固定原生构建目录（如 macOS CMake 目录 `target/macos-isolated`，勿作 `CARGO_TARGET_DIR` 或另起目录；产物坏了只删坏件重编，防多目录并行写满磁盘）。新起构建前跑 `df -h ~`，`< 50 GB` 禁在本机起新构建树（先清自有闲置产物或改走远端编译）。
- **安全清理**：合并或废弃后即跑 `git worktree remove <path>` 与 `git branch -D <branch>`（手删目录补 `git worktree prune`）。清理前必查：①`lsof +D <worktree>` 与 `pgrep` 查**整个 worktree 目录**（禁只查 `target/`，相对路径脚本在编译间隙可能未打开 `target/` 文件）；②分支已并入 `origin/develop`（祖先或 `git cherry origin/develop <branch>` 全已应用）且 `git status --porcelain` 无已跟踪修改，否则不动。
