# Clipshare Quick — クライアント側の作業パック

Claude Code が自走して Windows クライアントを実装するための資料一式。

| ファイル | 内容 |
|---|---|
| `SPEC.md` | 確定仕様（変更不可。変更したい場合は QUESTIONS.md） |
| `API_CONTRACT.md` | Clipshareとの契約（スナップショット。正本は `choko1229/clipeshare` main の同名ファイル） |
| `INSTRUCTIONS.md` | 実装指示（機能・チェック・レビュー・テスト） |
| `PROGRESS.md` | 進捗チェックリスト（自走の起点。常に更新） |
| `QUESTIONS.md` | 暫定判断と質問の記録 |
| `design/` | 画面デザインのスナップショット（クライアント画面 02–06, 08–12, 14–16, 18–19, 21–22） |

## 自走の手順
1. `CLAUDE.md` と上の資料を読む。
2. Clipshare側が未反映でもモックサーバー（契約準拠）で進める。
3. `PROGRESS.md` を上から。各項目: 実装→自動テスト→fmt/clippy/test→PROGRESS更新→コミット。
4. 実機でしか確認できない項目は `MANUAL_TESTS.md` に手順を書き「実機確認待ち」にする。
5. 全項目後にセルフレビューを2回以上行い、`REPORT.md` に完了報告を書く（INSTRUCTIONS §12）。
