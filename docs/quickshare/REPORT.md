# 完了報告（Clipshare Quick クライアント）

作成日: 2026-11-21 ／ ブランチ: `feature/installer`（`feature/ci-foundation` から順に積み上げ）

## 1. 構成図

```
                  ┌──────────────────────────── Windows（利用者のセッション） ────────────────────────────┐
                  │                                                                                        │
 VRChat ──写真──▶ │  写真フォルダ ──通知/再走査──▶ cq-core::watcher ──▶ cq-core::engine ──HTTPS──▶ Clipshare │
                  │                                   （安定待ち・重複防止）   │  ▲                (API v1)  │
                  │                                                         │  │                          │
 Explorer ─右クリック─▶ cq-shell-ext.dll ──名前付きパイプ（同一利用者のみ）──▶ cq-core::ipc                       │
                  │   (IExplorerCommand)                                    │                              │
                  │                                                         ▼                              │
                  │  clipshare-quick.exe（トレイ常駐・egui）◀── スナップショット／イベント                   │
                  │    ├ 画面: 初回設定・履歴・設定・進捗・更新・アンインストール                            │
                  │    ├ Windows 通知（WinRT）／クリップボード                                              │
                  │    ├ 資格情報マネージャー（トークン）                                                   │
                  │    └ cq-vr ──openvr_api.dll（実行時に読み込み）──▶ SteamVR（HUD・ダッシュボード）        │
                  │                                                                                        │
                  │  clipshare-quick-setup.exe ──PackageManager──▶ MSIX のインストール                       │
                  └────────────────────────────────────────────────────────────────────────────────────────┘
```

| クレート | 役割 |
|---|---|
| `cq-core` | 設定（検証・永続化・即時反映）、ログ（ローテーション・マスク）、API クライアント（契約 v1）、ブラウザ認可（PKCE・ループバック）、トークン保管、履歴・処理済み記録、フォルダ監視、アップロード（再試行・縮小）、エンジン、IPC サーバー、更新判定 |
| `cq-ipc` | パイプのプロトコル、SID 限定の DACL、サーバー所有者の検証つきクライアント（非同期ランタイムに依存しない） |
| `clipshare-quick` | トレイ常駐アプリと画面、通知、更新の適用、セットアップ |
| `cq-shell-ext` | 右クリック拡張 DLL |
| `cq-vr` | OpenVR の FnTable 呼び出し、HUD・パネルの描画と当たり判定 |
| `cq-mock-server` | 契約どおりのモック（全エラーコードを台本で再現） |

## 2. UI フレームワーク

**egui / eframe 0.31（glow）** を暫定採用（QUESTIONS Q-002）。純 Rust で C コンパイラ不要、MIT/Apache、トレイ常駐と同じスレッドで共存できる。
常駐中はウィンドウを持たず、画面を開くときだけ `run_and_return` でウィンドウを作る。日本語は Windows のシステムフォント（Yu Gothic UI → Meiryo）を使う。

## 3. 実装済み／未実装

（下の「セルフレビュー」「テスト結果」を参照。`PROGRESS.md` に項目ごとの状態がある）

| 項目 | 状態 |
|---|---|
| 基盤（ワークスペース・CI・ログ・設定・モック） | 実装・テスト済み |
| API クライアント・トークン保管・認可・アップロード・履歴 | 実装・テスト済み（資格情報マネージャーは実機で往復テスト） |
| フォルダ監視・VRChat フォルダ検出・クリップボード・同意 | 実装・テスト済み |
| 画面 04–06, 08–12, 15 | 実装済み。04・08/09・10 は実機で表示を確認、ほかは MANUAL_TESTS |
| 通知（16） | 実装・単体テスト済み、表示は実機確認待ち |
| 右クリック拡張（14）・SteamVR（18・19） | 実装・テスト済み、実機確認待ち |
| MSIX・セットアップ（02・03）・更新（21）・アンインストール（22） | 実装済み、実機確認待ち |
| リリースワークフロー | 下書き Release まで（公開は人） |

## 4. テスト結果（2026-11-21、開発機: Windows 11 build 26200、GNU ツールチェーン 1.100.0）

| チェック | 結果 |
|---|---|
| `cargo fmt --all -- --check` | 通過 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通過（`unwrap`/`expect`/`panic` は本番コードで禁止の設定） |
| `cargo test --workspace` | **112 件すべて成功**（4 回連続で実行して失敗なし） |
| `cargo build --workspace --release` | 成功（`clipshare-quick.exe` / `clipshare-quick-setup.exe` / `cq_shell_ext.dll` / `cq-mock-server.exe`） |
| `cargo audit` | 脆弱性 0 件。警告 4 件（推移的依存の unmaintained/unsound: Linux 専用の `glib`、`proc-macro-error`、`ttf-parser` など。Windows の実行経路では使われないか、代替が無い） |
| CI（GitHub Actions / MSVC） | PR で実行（ローカルは GNU。MSVC でのビルドは CI で確認） |

主なテスト（`crates/core/tests/engine_flows.rs` はモックサーバー相手の結合テスト 17 件）:
自動アップロード（URL コピー・履歴）、同意なし/自動 OFF では送らない、起動前の写真を無視、重複イベント・送信後の内容変更で再送しない、
413（画像は縮小して再送／動画はエラー）、上限の事前確認、429 日次（停止・停止中の写真は再開後も送らない・翌日再開）、
401（再ログイン要求・匿名で送らない・再ログイン後に再試行）、トークンだけ残った場合も匿名にしない、5xx/RATE_LIMITED/タイムアウトの再試行と失敗の永続化（再起動後も残る）、
オフライン（接続待ち → 自動で送信）、右クリック共有（選択順・改行区切り・一部失敗）、削除（匿名の削除トークン）、ログイン/ログアウト/拒否/コード交換失敗/キャンセル、
連続撮影のまとめコピー、アンインストールの後片付け。単体テストでは PKCE、ループバック受信（state 不一致の無視）、資格情報マネージャーの実機往復、
名前付きパイプ（DACL・二重起動）、パス検証（UNC・トラバーサル・リンク）、COM（クラスファクトリ・サブメニュー）、OpenVR のイベント構造、通知文言など。

実機での表示確認: 初回セットアップ（04）、履歴パネル（08/09、モック相手に実際の自動アップロード）、設定（10）、2 つ目の起動から IPC で画面を開く動作。

## 4.1 セルフレビュー

**1 回目（自分で INSTRUCTIONS §10 の各項目を照合）**: Authorization/X-Delete-Token ヘッダを機微情報扱いに、アンインストールで連携解除を選ばなくてもトークンと SteamVR 登録を削除、画面下部のボタン列の配置ずれを修正。

**2 回目（別のエージェントによる独立レビュー。こちらの判断を伝えずにコードと仕様だけを渡した）**: 15 件の指摘のうち 13 件を修正し、2 件を残課題として記録した。
- 修正: state.json が失われた場合に匿名で送ってしまう経路、送信後に内容が変わった写真の再送、自動 OFF 後の送信待ちの送信、監視し直しでの取りこぼし、右クリック共有の完了の二重通知、再試行成功時の通知なし、1日上限の通知の重複、トークンを読めない場合のログアウト結果、初回設定前の右クリック共有、右クリック拡張のスレッドと DLL のアンロード・応答待ちの期限、初回設定後のスタートアップ未反映、動画通知の文言、認可の受信待ち時間。
- 残課題: 更新ファイルの署名者の固定（Q-021）、パイプ名の先取りによるサービス妨害（Q-023）。
- 誤検知: `IVRApplications_008` は存在しない可能性 → `openvr_capi.h` で 008 を確認済み。

## 4.2 作業中に起きたこと（報告）

- 動作確認の途中で、こちらの手順ミス（設定ファイルを書き出すコマンドの誤り）により設定が不正になり、アプリが既定値（本番 URL）で起動した。その間に本番の `GET https://clipshare.link/api/v1/me` が 1 回、トークンなしで呼ばれた（404 が返った。アップロード・トークンの送信は無い）。すぐに停止し、以後は設定ファイルを検証してから起動した。
- エクスプローラー等の実機操作の確認中に Windows ファイアウォールの許可ダイアログ（Node.js、本作業とは無関係）が表示されたが、セキュリティ設定のため操作していない。
- シェルの書き間違いで空のファイルが 2 つリポジトリ直下に作られたが、コミット前に削除した。

## 5. 未検証項目の検証結果（SPEC §6・INSTRUCTIONS §11）

| 項目 | 結果 |
|---|---|
| VRChat `config.json` の出力先読み取り | `picture_output_folder` を読み、存在すれば優先、無ければ `ピクチャ\VRChat`、どちらも無ければ手動選択（テストで確認）。実際の VRChat の config.json での確認は MANUAL_TESTS §2 |
| 右クリック拡張の更新（Explorer 保持中） | `ERROR_PACKAGES_IN_USE` を受けたら「使用中なら登録を延期」に切り替える実装。実機確認待ち（MANUAL_TESTS §8） |
| MSIX 自動更新（.appinstaller）の GitHub Releases 運用 | 事前検証せず、自前の更新フロー（client/latest → ダウンロード → 検証 → PackageManager）で実装 |
| ライトテーマのコントラスト | デザインの配色を実装し、Windows の設定に追従。実機での目視確認待ち（MANUAL_TESTS §3） |
| 自己署名の信頼手順 | 手順書（installer/README.md）とセットアップの確認画面を用意。実機確認待ち |

## 6. リリース手順

`installer/README.md` を参照（証明書の作成 → `build-msix.ps1` → Secrets 登録 → タグ push → 下書き Release の確認・公開）。

## 7. 残課題・人に確認したいこと

QUESTIONS.md の Q-001〜Q-025（Q-004・Q-014・Q-015 は確認済み、ほかは未確認）。特に判断が必要なもの:

1. 署名（自己署名のまま公開するか、Publisher 名）— Q-006、Q-021
2. 配布物の形（セットアップ exe + msix か、msix のみか）— Q-008
3. アンインストールの導線（アプリ内から）— Q-009
4. Clipshare 側への依頼: client/latest の sha256 を必ず返すこと（Q-025）。動画の処理状態 API は無いことを確認済みで、現状仕様を維持（Q-015）
5. 403（BAN）時の文言・案内先（Q-024）
6. 開発機への VS Build Tools / Windows SDK の導入可否（入れれば MSIX のローカル作成・署名・インストール確認ができる）— Q-001

実機確認が必要な項目は `MANUAL_TESTS.md`（右クリック・SteamVR・MSIX・更新・アンインストール・VRChat 実撮影・ライト/ダーク・高 DPI）。
`main` へのマージ、タグの作成、GitHub Releases の公開、証明書の作成と信頼ストアへの登録は行っていない（手順は `installer/README.md`）。

## 8. API 契約 v1.1 への追従（2026-11-21）

`clipeshare` PR #1（`feature/quickshare-api-v1`）の `API_CONTRACT.md`（v1.1）と `REPORT.md` §7 を取り込んだ。すべて追加・明確化で、v1 の動作は壊していない。

| # | 契約の変更点 | クライアントの対応 | テスト |
|---|---|---|---|
| 1 | 認可URLに `device_platform`、各パラメータの形式 | `device_platform`（`Windows 11` / `Windows 10`、ビルド番号から判定）を付与。`device_name` は制御文字を除き前後の空白を取って 1〜64 文字（空なら `Windows PC`） | `auth` 単体、`v11_token_format_issuance_pause_and_ban_at_login` |
| 2 | 許可/キャンセルは 303 | ループバックはリダイレクトの種類に依存しない（GET を受けるだけ）。モックを 303 に変更 | 同上（303 を確認） |
| 3 | 任意の完了画面 `/qick/authorize/complete` | 許可を受けたらブラウザを `302 {base}/qick/authorize/complete` へ戻す（拒否・不正時は自前の短いページ） | `auth` 単体、同上 |
| 4 | トークンは `csq_` で始まる 47 文字。使用済みコードの再提示で連動失効 | 交換結果の形式を検証し、違えば受け取らない（保存しない）。コードは 1 回しか送らない | `api` 単体、同上（保存されたトークンの形式） |
| 5 | 403 は BAN（再認可で解決しない）、503 は発行停止中にも返る | 403 で「利用停止」状態にし、自動アップロードを止めて案内（再ログインを求めない・匿名にしない、Q-024）。連携時の 403/503 は専用の説明とコード（`FORBIDDEN` / `SERVICE_UNAVAILABLE`） | `v11_forbidden_blocks_uploads_without_falling_back_to_anonymous`、同上 |
| 6 | nginx の HTML 413、画素数超過 413（`limitPixels`） | 413 は本文が JSON でなくてもサイズ超過として扱い、画像は長辺 2000px に縮小して再送 | `v11_html_413_and_pixel_limit_are_shrunk_and_resent` |
| 7 | 429 の `details.dailyLimit` | 1日上限の通知の件数に使う（無ければ `/me` の上限） | `v11_daily_limit_details_are_used` |
| 8 | 日次はトークンありならユーザー単位（Web と共通）、匿名は IP 単位、リセットはサーバー現地 0 時 | 再開はサーバーの `resetsAt` に従う（既存の実装どおり）。件数表示は `/me` の `dailyUsed` を使うので Web 分も含まれる | 既存の日次上限テスト |
| 9 | 形式はマジックバイトで判定（jpg/png/webp/mp4/mov/webm/mkv/avi）、不一致は 415 | 送信前に拡張子と中身の一致を確認（不一致は送らない）。MKV・AVI を対象に追加（監視・右クリック・MSIX の関連付け） | `media` 単体、`v11_mock_rejects_nameless_part_and_mismatched_type` |
| 10 | 匿名は original でもバナーあり、original の寸法・品質 | 表示文言は既存のまま整合（未ログインは「画像にバナーが付きます」） | — |
| 11 | `deleteToken` は匿名のときだけ | トークンありの応答に含まれていても保存しない | `v11_token_format_issuance_pause_and_ban_at_login` |
| 12 | Bearer で削除できるのは API 経由のアップロードのみ | クライアントが削除するのは自分が API で上げたものだけ。404 は削除済みとして扱う（既存） | 既存の削除テスト |
| 13 | `client/latest` の `sha256` は null になりうる、未取得時 503 | null（または不正）ならアプリ内では適用せず、ダウンロードページへ案内（Q-025）。503 は確認失敗として記録 | `update` 単体 |
| 14 | `/api/v1/*` は CORS 不可・`no-store` | クライアントはブラウザから呼ばないので影響なし | — |
| 追加 | `file` はファイル名付きのパート（無いと 400） | 既存どおりファイル名を付けて送る。モックは無いと 400 | `v11_mock_rejects_nameless_part_and_mismatched_type` |

あわせて、連携アプリ管理ページの既定 URL を `{APIベースURL}/settings/apps` に修正した（Q-014）。動画の処理完了を知る API は無いため、`PROCESSING` のまま URL をコピーする現状仕様を維持した（Q-015）。

テスト結果（v1.1 追従後）: `cargo fmt --check`・`cargo clippy --workspace --all-targets -D warnings` 通過、`cargo test --workspace` **120 件成功**（4 回連続）、`cargo build --workspace --release` 成功。

## 9. オーナー回答の反映（2026-11-21）

### 9.1 決定事項の実装

| 決定 | 実装 | テスト |
|---|---|---|
| Q-011 既定の画質は縮小 | `Settings::default` と `Quality::default` を縮小に。保存済みの設定は必ず `defaultQuality` を含むため既存の利用者は変わらない（マイグレーション不要）。右クリックの「（既定）」と、設定を読めない場合の既定も縮小 | `default_quality_is_shrink_but_saved_choice_is_kept`、`menu` 単体、結合テスト（既定で `quality=shrink`） |
| Q-024 403 はお問い合わせへ | 通知・履歴パネル・設定アカウント・初回セットアップの失敗画面に「お問い合わせ」。403 では「ログインして連携」「もう一度試す」「再ログイン」「再試行」を出さない。案内先は `advanced.contactPath`（既定は暫定値 `/contact`、Q-027） | `forbidden_points_to_contact_page_only`、`v11_forbidden_blocks_uploads...`（再試行できないことを追加） |
| Q-025 sha256 は必ず返る（契約 v1.2） | 既存の実装が一致（あれば必ず検証し、不一致は破棄・エラー表示、null は手動案内）。モック相手に 3 ケースと未取得 503 を確認 | `v12_update_sha256_present_null_and_mismatch` |
| Q-006/Q-008 自己署名・2 ファイル配布 | 公開証明書をセットアップに埋め込む（`CQ_SIGNING_CER`、`build.rs`）。Publisher を `installer/signing.json` に集約（仮値、Q-026）。release.yml は `.pfx` の Subject と一致を確認。信頼する証明書が無ければ `CERT_NOT_FOUND`。再試行時は署名を確認し直して信頼の手順の要否を決める | 手順は MANUAL_TESTS §9.1 |
| Q-016 削除用トークンは履歴ファイル | 監査（下記）の結果、期限切れ時の削除・一時ファイル・壊れたファイル・フォルダの権限を強化 | `delete_tokens_are_purged_when_unusable_and_never_printed`、`broken_history_is_discarded_not_kept`、`stale_tmp_files_are_removed`、`data_dir_acl_is_restricted_and_still_usable` |
| Q-007/Q-009/Q-012 | 現状維持・採用。Q-012 は `SPEC.md` §8 に確定仕様として記録、デザイン更新の依頼は `design/README.md` | — |

### 9.2 削除用トークン（Q-016）の監査結果

| 観点 | 結果 | 対応 |
|---|---|---|
| ログ・クラッシュ出力・エラー文言 | トークンは `SecretString`（`Debug`/`Display` は `***`）。使うのは削除 API のヘッダ（`set_sensitive`）だけで、ログ・エラーには出ない。パニックの記録は発生箇所のみ | 問題なし（`Debug` に出ないテストを追加） |
| 削除成功後 | `delete_remote` でトークンを消して保存していた | 問題なし |
| 期限切れ後 | **消していなかった** | 読み込み時と 30 秒ごとに、期限切れ・削除済み・失敗の項目のトークンを消して保存 |
| 書き込み途中の一時ファイル | 異常終了時に `.history.json.<pid>.tmp` が残り得た | 読み込み時に削除。アンインストール時も削除 |
| 破損時 | `history.broken.json` に退避していた（トークンを含み得る） | 退避せず削除して空の履歴で起動（トークンは 24 時間で無効、用途は自分の匿名アップロードの削除のみ） |
| ファイルの権限 | `%LOCALAPPDATA%` から継承。開発機ではアプリコンテナーの SID や別グループにも読み取りが継承されていた | データフォルダに保護された DACL（利用者・SYSTEM・Administrators のみ）を起動時に設定。エクスプローラー（同じ利用者）からは読める |

### 9.3 セルフレビュー（今回の変更分）

1. **1 回目（型・エラー処理・ログ）**: 新しい設定値（`contactPath`）は同じホストのパスだけを許可する検証に追加。ACL 設定の失敗は起動を止めず警告ログ。`build.rs` は証明書を読めない場合に警告して空で続行。unsafe ブロックはすべて SAFETY コメントつき（DACL の確保・解放、SetNamedSecurityInfoW）。
2. **2 回目（オーナー決定との照合・セキュリティ）**: 403 で再認証を促す箇所が 2 つ残っていた（設定アカウントの「ログインして連携」、履歴パネルの「ログインして制限を解除」）→ 利用停止中・403 の失敗時は出さないよう修正。秘密情報（`.pfx`・`.cer`・`.pem` など）がリポジトリに含まれていないことを確認し、`.gitignore` に証明書ファイルを追加。
3. **3 回目（失敗時の再試行）**: セットアップで信頼に成功した後にインストールだけ失敗した場合、再試行で消した一時証明書を探して `CERT_NOT_FOUND` になる経路を発見 → 再試行時に署名を確認し直し、信頼の手順が要るときだけ行うよう修正。一時証明書は信頼に成功した後だけ削除。

### 9.4 テスト結果（GNU ツールチェーン 1.100.0、ローカル）

`cargo fmt --all -- --check` 通過、`cargo clippy --workspace --all-targets -- -D warnings` 通過、`cargo test --workspace` **127 件成功**（3 回連続）。`cargo build --workspace --release` 成功。CI（MSVC）は PR で実行。

### 9.5 オーナー待ち
- Q-026 Publisher 名（仮値 `CN=Clipshare Quick`、`installer/signing.json`）。証明書を作る前に決める必要がある → **2026-11-21 確定 `CN=choko1229`（§10）**
- Q-027 403 の案内先 URL（暫定値 `{APIベースURL}/contact`、`DEFAULT_CONTACT_PATH`）→ **2026-11-21 確定 `https://clipshare.link/contact`（§10）**

## 10. Publisher・お問い合わせ URL の確定と SHA256SUMS（2026-11-21）

### 10.1 変更内容

| 項目 | 実装 | 確認 |
|---|---|---|
| Q-026 Publisher | `installer/signing.json` を `CN=choko1229` に。仮値 `CN=Clipshare Quick` はリポジトリに残っていない（QUESTIONS・REPORT の経緯の記述のみ）。`release.yml` の Subject 照合を大文字小文字も区別する比較（`-cne`）に | メモリ上だけで作った一時的な自己署名証明書（Subject = signing.json の値）を `.pfx` に書き出して読み直し、`Subject -ceq publisher` が真（証明書ストアは使っていない） |
| Q-027 お問い合わせ URL | 既定の `DEFAULT_API_BASE_URL` + `DEFAULT_CONTACT_PATH` = `https://clipshare.link/contact`。コメントを確定値に | `actions::tests::default_contact_url_is_clipshare_contact_page`、`join_web_ignores_trailing_slash_of_base` |
| SHA256SUMS（最優先） | `installer/release-checksums.ps1`（作成・検証）。`release.yml` で exe と MSIX の 2 ファイルだけから作り、3 ファイルを下書きに添付 → 下書きから取り直して照合 → 失敗・中止なら下書きを削除。同じタグの Release があれば開始しない | `installer/test-release-checksums.ps1` 20 件（形式、欠落、余分な `.appinstaller`、余分な行、CRLF、BOM、大文字の hex、空白 1 つ、順序、名前の大文字小文字、改ざん、digest の不一致・null・欠落）。CI とリリースで実行 |
| 形式 | Clipshare の契約 v1.2 がクライアントから参照できないため、sha256sum 互換 `<64桁hex>  <ファイル名>`（LF、BOM なし、exe → MSIX の順）を採用 | QUESTIONS Q-028。`API_CONTRACT.md` の先頭に「v1.2 のマージ後に照合」の注記 |
| `.ps1` の文字コード | `installer/*.ps1` を BOM 付き UTF-8 に | Windows PowerShell 5.1 で日本語を含むスクリプトの構文解析・実行が通る |

### 10.2 セルフレビュー（今回の変更分）

1. **1 回目（要件との照合）**: 対象が 2 ファイルだけ・ファイル名が添付ファイル名と完全一致・`.appinstaller` などを含めない・不一致や欠落で失敗、を 1 つずつテストに対応させた。GitHub の digest も照合（Clipshare は digest と SHA256SUMS の一致を配布の条件にするため、ここで先に落とす）。digest が無い場合も失敗にした（安全側。Clipshare 側で配布されない状態を早く知らせる）。
2. **2 回目（ワークフローの失敗経路）**: 下書きを作った後に実行が「中止」された場合に、照合していない下書きが残る経路を発見 → 削除の条件を `failure() || cancelled()` に。削除してよいのは「この実行で作った下書き」だけにするため、開始前に同じタグの Release が無いことを確認する手順を追加（公開済みの Release は対象外）。添付ファイル名は `^[A-Za-z0-9._-]+$` に限定して保存先のパスに使う。添付ファイルの取得はトークンつきの API から行い、リダイレクト先へ Authorization を送らない（PowerShell 7 の既定）。トークンはログに出さない。
3. **3 回目（他の環境・既存ファイル）**: 既存の `new-dev-certificate.ps1`・`build-msix.ps1` が BOM 無しの UTF-8 で、オーナーが手元の Windows PowerShell 5.1 で実行すると日本語の文字列が ANSI として読まれ、構文が壊れ得ることを発見 → BOM 付きに。`release.yml` の各 `pwsh` ステップを PowerShell の構文解析器で確認し、YAML として読めることも確認。オーナーの回答の Q-021・Q-022 は Clipshare 側の番号（このリポジトリの Q-021 は署名者の固定、Q-022 は初回セットアップ前の右クリック）と判断し、このリポジトリの状態は変えずに Q-021 の補足と Q-028 に記録した。

### 10.3 テスト結果（GNU ツールチェーン 1.100.0、ローカル）

`cargo fmt --all -- --check` 通過、`cargo clippy --workspace --all-targets --locked -- -D warnings` 通過、`cargo test --workspace --locked` **129 件成功**、`cargo build --workspace --release --locked` 成功。`installer/test-release-checksums.ps1` 20 件成功（Windows PowerShell 5.1。CI は PowerShell 7）。`release.yml` の実行（タグの push）は行っていない（リリースの作成はオーナーの作業）。

### 10.4 オーナーの作業（この順）

1. PR（`feature/installer`）を確認して `main` にマージする。
2. 証明書を作る（`CN=choko1229`）: `.\installer\new-dev-certificate.ps1 -OutDir C:\secure`。`.pfx` はリポジトリの外で秘密に保管する。
3. GitHub の Secrets に `MSIX_PFX_BASE64`・`MSIX_PFX_PASSWORD` を登録する（`installer/README.md` §4）。
4. `CHANGELOG.md` と `Cargo.toml` のバージョンを確定し、`main` でタグ `vX.Y.Z` を作って push する。
5. Actions の「Release」の成功を確認し、下書きの添付ファイルが 3 つ（exe・MSIX・SHA256SUMS）であることを確認して公開する（MANUAL_TESTS §9.3）。
6. 実機確認（MANUAL_TESTS 全体。特に §9.1 自己署名の導線、§9.3 SHA256SUMS、右クリック、SteamVR、VRChat 実撮影）。
7. Clipshare 側の契約 v1.2 がマージされたら、`API_CONTRACT.md` を差し替えて SHA256SUMS の形式を照合する（Q-028）。

## 11. 更新版デザインへの追従（2026-11-22）

### 11.1 進め方
1. 更新版 11 枚（06, 08, 09, 10, 11, 12, 14, 16, 21, 23, 24）の HTML から文言・ボタン・トグル（`aria-checked`）・選択状態を取り出し、前の版との差分と実装を突き合わせて `DESIGN_DIFF.md` を作った（コード変更の前にコミット）。
2. デザインのとおりにすると既存の決定や仕様とぶつかる 3 点（Q-029 設定アカウントの 403 でログアウトが消える、Q-030 履歴パネルの 403 で一覧が消える、Q-032 エクスプローラーのメニューにバッジが描けない）は実装せずに質問した。仮置き文言（「GitHubを開く」「開く」、未完了通知の本文）はそのまま採用し、文言の提案は Q-033。
3. 未実装分を実装した（`DESIGN_DIFF.md` の「対応結果」）。既存の決定（Publisher、2 ファイル配布、アプリ内アンインストール、既定は縮小、削除用トークンは履歴ファイル、403 はお問い合わせ、503 は発行停止の表示）は変えていない。

### 11.2 変更ファイル
- core: `engine.rs`（`TrayState::Blocked`・`TrayState::resolve`・`TrayFacts`、`Snapshot::auto_upload` を実際の状態に、`auto_upload_setting` を追加）、`update.rs`（`ManualOnly` に容量）、`settings.rs`（既定値のテスト）、`tests/engine_flows.rs`
- app: `ui/settings.rs`（一般・情報・通知の一覧化、403 のトグル、ログアウトの通知）、`ui/update.rs`（⑮⑯）、`ui/history.rs`、`ui/wizard.rs`（`failure_action`）、`ui/widgets.rs`（`small_danger`）、`toast.rs`、`updater.rs`、`tray.rs`、`vr_bridge.rs`、`actions.rs`（`toggle_auto_upload`・`SOURCE_URL`）、`app.rs`
- docs: `DESIGN_DIFF.md`（新規）、`QUESTIONS.md`（Q-029〜Q-033）、`MANUAL_TESTS.md` §11、`PROGRESS.md` Step 7、`CHANGELOG.md`、`design/`（更新版の取り込み）、`design/README.md`

### 11.3 セルフレビュー
1. **1 回目（デザインとの一致）**: 抽出したテキストと `aria-checked` を画面ごとに照合。12 の「更新を自動で確認」はバッジがトグルの左（右から並べるレイアウトで順序を合わせた）。23 も区切り線の一覧に揃えた。
2. **2 回目（状態の整合・エラー処理）**: 403 で自動アップロードを「OFF 表示」にしたところ、トレイのメニュー・VR パネルの切り替えが「設定値を反転」していたため、表示が OFF のまま裏で設定を OFF にしてしまう経路を発見 → 切り替えの判定を `actions::next_auto_upload` に集約し、403 の間は何もしない（トレイの項目も無効化）。設定の保存失敗・クリップボードの失敗は警告ログに残す（値は出さない）。
3. **3 回目（セキュリティ・ログ・型）**: 追加した通知・ログにトークン・URL のクエリ・ファイル内容は含まれない。ソースコードの URL は定数（`https` のみを開く `open_url` を通る）。`TrayState` の追加で網羅性チェック（`match`）がトレイのアイコン・履歴のバッジの更新漏れを検出した。`unwrap`/`expect` は本番経路に追加していない。

### 11.4 テスト結果（GNU ツールチェーン 1.100.0、ローカル）
`cargo fmt --all -- --check` 通過、`cargo clippy --workspace --all-targets --locked -- -D warnings` 通過、`cargo test --workspace --locked` **138 件成功**（追加 9 件）、`cargo build --workspace --release --locked` 成功、`installer/test-release-checksums.ps1` 20 件成功。push はしていない（ローカルのコミットのみ）。

### 11.5 未解決の質問
- Q-028 SHA256SUMS の形式（Clipshare 側の契約 v1.2 のマージ待ち）
- Q-029 設定アカウントの 403 でアカウント欄・ログアウトを消すか（現状は残す）
- Q-030 履歴パネルの 403 で一覧を消すか（現状は残す）
- Q-031 403 の間のトグル表示（暫定案で実装。変更があれば指示がほしい）
- Q-032 右クリックの「既定」バッジ・補足（プラットフォームの制約で「（既定）」のまま）
- Q-033 「連携の解除が未完了」の本文（デザインの文言を採用。理由を限定しない文言を提案）

### 11.6 オーナー回答の反映（2026-11-22）
- Q-029〜Q-032: 実装のまま確定。デザインのスナップショット（`design/09`・`11`・`14`）を実装に合わせて更新し、キャンバスへの反映を `design/README.md` の依頼 13〜15 に追加。
- Q-031 の確認中に、ヘッダーのバッジ「利用不可」の色がデザイン 09（灰色）と違う（赤で実装していた）ことに気づき、灰色に修正（`history::header_tone`）。
- Q-033: 通知の本文を「連携の解除を完了できませんでした。連携アプリで状態を確認してください。」に変更（実装・テスト・`design/16`・依頼 16）。画面 22 の文言は質問の対象外のため変えていない。
- Q-028: Clipshare 側の契約 v1.2 のマージ後に突き合わせる（未完了）。
- テスト: `cargo fmt --all -- --check` 通過、`cargo clippy --workspace --all-targets --locked -- -D warnings` 通過、`cargo test --workspace --locked` **139 件成功**、`installer/test-release-checksums.ps1` 20 件成功。コミットはローカルのみ。
