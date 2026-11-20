# CLAUDE.md（clipshare-qick）

Clipshare Quick: VRChat の写真を自動で Clipshare.link へアップロードし、共有URLをクリップボードへ入れる Windows 常駐アプリ（Rust）。MIT、公開リポジトリ。

## 作業の前に必ず読む（この順）
`docs/quickshare/README.md` → `SPEC.md` → `API_CONTRACT.md` → `INSTRUCTIONS.md` → `PROGRESS.md` → `QUESTIONS.md`。画面は `docs/quickshare/design/`（スナップショット）と https://claude.ai/artifact/79AJ4vKmEg1c8MffHqwYLG が正。

## 前提
- Clipshare側（リポジトリ `choko1229/clipeshare`）が先に実装される。API契約の正本は同リポジトリ main の `docs/quickshare/API_CONTRACT.md`。**この間は契約書どおりのモックサーバーで開発・テストする**。Clipshare側が未反映でも止まらない。契約が更新されていたら（`gh api` / `git` で公開リポジトリの該当ファイルを取得して確認）追従する。

## 標準の開発ルール（常に適用）
- 設定はハードコードせず永続化し、UIから変更可。`.env` 相当は最小限。
- 型を厳密に。本番経路で `unwrap`/`expect` を使わない。`unsafe` は最小限で理由コメント必須。
- エラー処理・例外処理・詳細ログ。**ログにトークン、Authorization、ファイルの内容を出さない。**
- セキュリティ重視。保守性・可読性・拡張性を最優先。品質を速度より優先し、妥協しない。
- 仕様に無い機能を足さない（QR表示、手首オーバーレイ、OSC連携は対象外）。UIに動作説明の文章を足さない。日本語のみ。
- 完成後に複数回セルフレビュー（INSTRUCTIONS §10）。

## 自走ルール
- 質問で止まらない。仕様の穴は `QUESTIONS.md` に「質問・採用した暫定案・影響範囲」を書き、**最も安全で後から直しやすい案**で進む。`SPEC.md` の確定事項に反する案は採らない。
- 止まってよいのは取り消せない操作のみ: `main` への push/マージ、リリース公開（タグ・GitHub Releases）、署名証明書や秘密情報の取り扱い、本番Clipshareへの実アップロード（テスト用の本番アカウントが無い場合）、ユーザーのシステム全体に影響する操作（レジストリ/MSIXの実インストールは**開発用の隔離手順**のみ）。これらは手順書にして報告する。
- 作業は `PROGRESS.md` の順に進め、1項目ごとに更新・コミット。中断しても再開できる状態を保つ。
- 各項目の完了条件: `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`、Windowsでの `cargo build --release` が通ること。通らないまま次へ進まない。
- Windows実機でしか確認できない項目（右クリック、SteamVR、MSIX、VRChat実撮影）は、実装とテスト可能な範囲の自動テストを済ませ、**手動確認手順書**（`docs/quickshare/MANUAL_TESTS.md`）を作って `PROGRESS.md` に「実機確認待ち」と記録する。

## ブランチ・コミット
- `main` へ直接コミットしない。機能ごとに `feature/<name>`（小さく刻む）、論理単位でコミット（Conventional Commits、日本語可）。pushは作業ブランチのみ可。PRは作成可、マージはしない。
- CI（GitHub Actions）を最初に整備: fmt / clippy / test / `cargo audit`（または deny）/ Windowsビルド。
- 初回のみ `main` に初期整備（LICENSE・.gitignore は配置済み）を載せる必要がある場合は、ユーザー確認待ちとして報告する。

## コミットメッセージの末尾
Co-Authored-By: Claude <noreply@anthropic.com>
