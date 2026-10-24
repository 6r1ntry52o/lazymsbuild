# lazymsbuild

[English](README.md) | 日本語

[lazygit](https://github.com/jesseduffield/lazygit) 風の MSBuild 用ターミナル UI です。`compile_commands.json` を常に完全な状態に保ち、clangd（と clang-tidy、それらを使うすべてのエディタ）が Visual Studio の C++ ソリューションを、インクルードパスも定義もすべてのファイルも含めて理解できるようにします。

```text
╭[1] Solution & Targets────────────╮╭[0] Log | Errors (E:0 W:0)────────────────────────────────╮
│> [Solution] Demo.sln ●           ││  core.cpp                                                │
│  ├─ Libs/                        ││  engine.cpp                                              │
│  │   └─ CoreEngine               ││  CoreEngine.vcxproj -> C:\src\demo\x64\Debug\CoreEngine.l│
│  ├─ Tools/                       ││ib                                                        │
│  │   └─ Updater                  ││  updater.c                                               │
│  └─ ControlApp                   ││  Updater.vcxproj -> C:\src\demo\x64\Debug\Updater.exe    │
│                                  ││  pch.cpp                                                 │
╰ target: [Solution] Demo.sln ─────╯│  main.cpp                                                │
╭[2] Configuration / Platform──────╮│  window.cpp                                              │
│          Win32  x64  ARM64       ││  ControlApp.vcxproj -> C:\src\demo\x64\Debug\ControlApp.e│
│ Debug      ·     ●     ·         ││xe                                                        │
│ Release    ·     ◆               ││Rebuild succeeded in 8.4s: 0 error(s), 0 warning(s)       │
╰ ● active  ◆ database ────────────╯╰──────────────────────────────────────────────────────────╯
╭[3] Actions & Options─────────────╮╭clangd DB Status──────────────────────────────────────────╮
│ b Build                          ││Profile  : Debug|x64            Mode     : repo-root      │
│ r Rebuild                        ││Entries  : 6 (+6)               Coverage : 6/6            │
│ c Clean                          ││Stale    : none                 Updated  : 12:03:11       │
│ s Sync (no build)                ││Source   : build 6              Output   : ✔ published    │
│ m Parallel (-m) : ON             ││File     : compile_commands.json                          │
╰──────────────────────────────────╯╰──────────────────────────────────────────────────────────╯
b build · r rebuild · c clean · s sync · x menu · ? help · q quit
```

- **Visual Studio と同じようにビルドする。** 入口は常にソリューションなので、ソリューション構成の対応付けがそのまま効きます。プロジェクト単体のビルドもソリューションのターゲット経由です。IDE と同じく、Visual Studio 2022 の 64 ビット版 MSBuild とコンパイラを使います。
- **ビルドしなくても完全な DB。** `lazymsbuild sync` が全プロジェクトのデザインタイム評価から DB を作ります。以後はビルドのたびに、コンパイルしたファイルのエントリを、MSBuild が実際に実行したコマンドラインで更新します。
- **clang のツールが探す場所に置く。** `compile_commands.json` はリポジトリのルートに置くので、clangd も clang-tidy も自分で見つけます。`git status` には出ません。
- **「最新か？」に答える。** `lazymsbuild check` はカバレッジ、古くなったプロジェクト、clangd が代わりに読んでしまう DB を報告します。`lazymsbuild doctor` は clangd までの経路全体を確かめます。
- **ビルドは独立して走る。** 状態は `status.json` にあるので、TUI が見えていてもいなくても、エディタが状態を表示し、終了を通知できます。
- **何も外に出さない。** テレメトリ、ネットワーク通信、自動更新はありません。

## 動作環境

- Windows 10 / 11（x64 または ARM64）
- Visual Studio 2019 以降、または Build Tools（「C++ によるデスクトップ開発」）。動作確認は Visual Studio 2022 で行っています
- `.sln` / `.slnx` の中の C++ プロジェクト（`.vcxproj`）。プラットフォームは x86・x64・ARM64
- `sync` には Microsoft の [msbuild-extractor-sample](https://github.com/microsoft/msbuild-extractor-sample)（`scripts/install-extractor.ps1` で入ります）。無い場合はリビルドで DB を埋めます
- 結果を使うための clangd

## インストール

- **GitHub Releases**: `lazymsbuild-v<version>-x86_64-pc-windows-msvc.zip`（または `aarch64-pc-windows-msvc`）をダウンロードして `SHA256SUMS` と照合し（`Get-FileHash <zip> -Algorithm SHA256`）、`lazymsbuild.exe` と `LazyMsBuild.Logger.dll`（exe の隣から動かさない）を `PATH` の通ったディレクトリに置きます。`sync` 用の `install-extractor.ps1` も同梱しています。
- **cargo**: `cargo binstall lazymsbuild` はリリースの exe をダウンロードし、`cargo install lazymsbuild` はビルドします。どちらも入るのは `lazymsbuild.exe` だけです。ビルドで DB を更新するには、`LazyMsBuild.Logger.dll`（リリースのアーカイブにあります）を exe の隣に置くか、クローンで `dotnet build logger -c Release` を実行してロガーを作り、`extractor.logger` でその場所を指定します（[docs/config.md](docs/config.md)）。見つかっているかは `lazymsbuild doctor` で分かります。
- **[dotfiles-windows](https://github.com/6r1ntry52o/dotfiles-windows)**: `install.ps1` がバージョンを固定したリリースを入れ、Neovim を設定します。

## はじめかた

ソリューションのあるリポジトリで:

1. `lazymsbuild init` が `.clangd` を書き（既にあれば書きません）、生成されるファイルを Git の対象から外します（`.git/info/exclude`。`--ignore gitignore` なら確認のうえ `.gitignore` に書きます）。
2. `lazymsbuild sync` が、ビルドせずに現在の構成の DB を埋めます。デザインタイムの抽出器が無い場合は、TUI（`lazymsbuild`）を起動してソリューション全体を一度リビルドします。
3. `lazymsbuild check` が、カバレッジ 100% で古いプロジェクトが無いことを報告するはずです。
4. エディタで clangd を有効にします。ほかの設定は要りません。clangd はルートの DB を自分で見つけます。LazyVim と VS Code は [`docs/editors/`](docs/editors/) を見てください。
5. `lazymsbuild doctor` に失敗が出ないはずです。

以後は TUI か `lazymsbuild build` でビルドすれば、DB が追従します。

## TUI

引数なしの `lazymsbuild` で起動します。ペイン 1 でターゲット（ソリューション全体か、プロジェクト 1 つ。選んだものは一覧の下に表示されます）を、ペイン 2 で構成を選んでビルドします。キーは lazygit に合わせてあり、設定の `[keys]` ですべて変えられます。

| キー | 動作 |
|---|---|
| `1`-`3`, `Tab`, `Shift+Tab`, `h`, `l` | 左のペインの切り替え |
| `0`, `Esc` | ログ（Log / Errors）へ移る、戻る |
| `j`, `k`, `↑`, `↓`, `g`, `G`, `<`, `>`, `,`, `.` | 移動、ページ送り（構成の表では Platform の列を上から下へ、続けて次の列へ） |
| `J`, `K`, `Ctrl+d`, `Ctrl+u`, `PgDn`, `PgUp` | どのペインからでもログをスクロール |
| `[`, `]` | ペイン内のタブの切り替え（Log / Errors） |
| `Enter`, `Space` | ターゲットや構成を選ぶ |
| `/` | 一覧の絞り込み |
| `?` | すべてのキーを表示 |
| `x` | すべての操作のメニュー |
| `b`, `r`, `c` | ビルド、リビルド、クリーン（クリーンは確認あり） |
| `s` | 同期（ビルドせずに DB を埋める） |
| `m` | 並列ビルド（`-m`）の切り替え |
| `e` | 選んだエラーをエディタでその行に開く（`ui.edit_at_line`、無ければ `$EDITOR`、`$VISUAL`、既定のアプリ） |
| `Ctrl+c` | ビルドの中止（確認あり）。何も走っていなければ終了 |
| `Esc` | 絞り込みやメニューを閉じる、ログから戻る |
| `q` | 終了（ビルド中は確認あり） |

ビルド中も、移動、ログのスクロール、DB の状態の確認ができます。別のビルド、構成の切り替え、クリーン、同期は、ビルドが終わるまで待ちます。

## コマンド

| コマンド | 内容 |
|---|---|
| `lazymsbuild` | TUI を起動する |
| `lazymsbuild init [--mode repo-root\|external] [--ignore exclude\|gitignore\|none]` | `.clangd` を書き、生成されるファイルを Git の対象から外す |
| `lazymsbuild sync [--profile C\|P]` | ビルドせずに DB を埋める |
| `lazymsbuild build [--target T] [--profile C\|P] [--rebuild\|--clean]` | TUI なしでビルドし、DB を更新する |
| `lazymsbuild check [--profile C\|P]` | DB は完全で最新か |
| `lazymsbuild doctor [--report]` | clangd に必要なものは揃っているか（`--report` はパスを伏せる。issue 用） |
| `lazymsbuild path [--db\|--state\|--config]` | 各ファイルの場所（エディタはパスを自前で組み立てず、これを呼ぶ） |
| `lazymsbuild completions <shell>` | シェル補完スクリプト（powershell, bash, zsh, fish, elvish） |

共通のオプション: `--json`（機械向けの出力）、`-C <dir>`（`git -C` と同じ）、`--solution <file>`（リポジトリに複数あるとき。以後のコマンドとエディタも同じものを使います）、`-q`、`-v`。終了コード: `0` 成功、`1` 失敗（ビルドの失敗、問題の検出）、`2` 使い方の誤り。構成は `Configuration|Platform` と書き、シェルでは引用符で囲みます（`--profile "Release|x64"`）。`--profile` を付けると、その構成が現在の構成にもなります。

## 置き場所

リポジトリの中（既定の repo-root モード）:

| ファイル | |
|---|---|
| `compile_commands.json` | 現在の構成の DB。`output.path` で `build/` などへ移せます。clangd はそこでも見つけますが、clang-tidy には `-p build` が要ります |
| `.clangd` | 無ければ `init` が書きます。MSVC のプリコンパイル済みヘッダーのフラグを clangd に無視させます |
| `.cache/clangd/` | clangd のインデックス（clangd が作ります） |

`init` はこれらを `.git/info/exclude` に書きます。クローンごとのローカルな設定なので、追跡されているファイルは変わりません。既に追跡されている `compile_commands.json` や `.clangd` には書き込みません。

リポジトリに何も置けない場合は、`lazymsbuild init --mode external` で DB を状態ディレクトリに置きます。その場合、DB を使えるのは `--compile-commands-dir=$(lazymsbuild path --db)` を設定したエディタだけで、clang-tidy は見つけられません。

リポジトリの外:

| パス | |
|---|---|
| `%APPDATA%\lazymsbuild\config.toml` | 個人の設定（`%XDG_CONFIG_HOME%\lazymsbuild` か `%LAZYMSBUILD_CONFIG_DIR%` でも可） |
| リポジトリのルートの `.lazymsbuild.toml` | チームで共有する設定（任意。コミットする） |
| `%LOCALAPPDATA%\lazymsbuild\<repo>-<hash>\` | 状態: 構成ごとの DB、`status.json`、`errors.txt`、ログ。消しても構いません |

## DB を完全に保つしくみ

MSBuild からコンパイルコマンドを取り出す方法は 2 つあり、lazymsbuild は両方を使います。

- **デザインタイム（`sync`）**: Microsoft の msbuild-extractor-sample が、ビルドせずに全プロジェクトを評価します。完全ですが、内容は最後に実行した時点のものです。
- **ビルド時**: lazymsbuild の MSBuild ロガーが、毎回のビルドのコマンドラインを記録します。正確で追加のコストもかかりませんが、ビルドは変更のあったものしかコンパイルしません。

`sync` が構成の DB を作り、以後はビルドのたびに、コンパイルしたファイルのエントリを更新し、リビルドに成功したプロジェクトのエントリを置き換え、プロジェクトから外れたファイルのエントリを消します。MSBuild は一部の設定（*VC++ ディレクトリ*、ターゲットのアーキテクチャ）をコマンドラインの外で cl.exe に渡すので、lazymsbuild はそれをエントリに足し、clang のツールにコンパイラと同じものが見えるようにします。clang のツールは MSVC のプリコンパイル済みヘッダーを使えないので、それを使うエントリには `/Y-` も足し、clang-tidy でも動くようにします。詳細と根拠の計測は `docs/design.md`（英語）にあります。

## プライバシー

lazymsbuild はネットワークに接続せず、何も収集せず、自分を更新しません。ログは状態ディレクトリに残るだけです。issue を報告するときは、`lazymsbuild doctor --report`（パスを伏せ、プロジェクト名を含まない）と、問題を再現する最小のソリューションを添えてください。会社のログやソリューションは添付しないでください。

## トラブルシューティング

- **`doctor` が hostfxr.dll について警告する**: デザインタイムの抽出器は、Visual Studio の MSBuild を見つけられないとき、隣に .NET の `hostfxr.dll` が必要です。`scripts/install-extractor.ps1` が置きます。
- **「... is tracked by Git」**: リポジトリが `compile_commands.json` をコミットしています。`lazymsbuild init --mode external` を使うか、ファイルの追跡をやめてください。
- **「... was not written by lazymsbuild」**: 別のツールが `compile_commands.json` を書いています。置き換えるには `--force` を付けます（TUI では確認が出ます）。
- **`check` の「other databases」**: clangd が読むかもしれない古い `build/compile_commands.json`（たとえば `:CompileCommands` の設定が作ったもの）があります。消してください。
- **ある構成でプロジェクト単体をビルドできない**: そのソリューション構成ではそのプロジェクトをビルドしないので、MSBuild にそのターゲットがありません。

## ドキュメント（英語）

- [docs/config.md](docs/config.md): すべての設定、キー、環境変数
- [docs/editors/lazyvim.md](docs/editors/lazyvim.md): LazyVim（clangd、`:CompileCommands`、quickfix、ビルド終了の通知、フローティングターミナルでの TUI）
- [docs/editors/vscode.md](docs/editors/vscode.md): VS Code（clangd 拡張とビルドタスク）
- [docs/formats.md](docs/formats.md): `lazymsbuild path`、`status.json`、`errors.txt`、DB のファイル（エディタ連携やスクリプト向け）
- [docs/design.md](docs/design.md): しくみとその理由、根拠の計測

## コントリビュート

[CONTRIBUTING.md](CONTRIBUTING.md)（英語）を見てください。`tests/fixtures/` のテスト用ソリューションは合成したものです。今後もそうしてください。

## ライセンス

[Apache License, Version 2.0](LICENSE-APACHE) か [MIT license](LICENSE-MIT) のどちらかを選べます。サードパーティの表示: [THIRD_PARTY.md](THIRD_PARTY.md)。
