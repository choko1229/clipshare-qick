# API 契約 v1（Clipshare ⇔ Clipshare Quick）

**正本は `clipeshare` リポジトリ（main）の `docs/quickshare/API_CONTRACT.md`。** `clipshare-qick` 側のコピーはスナップショット。変更は**追加のみ**（互換維持）。破壊的変更が必要なら実装せず `QUESTIONS.md` に書く。Clipshare側が変更・追加したら末尾「変更履歴」に追記する。
本書は設計案であり、既存実装（`POST /api/qick/upload` 等）との整合をClipshare側Claude Codeがコードで確認し、**差異があればこの文書を更新**してよい（その場合は変更履歴に理由を書く）。

- ベースURL: `https://clipshare.link`（既存の本番URL。サイトURL設定値を確認して合わせる）
- 全API: HTTPS、`User-Agent: ClipshareQuick/<version> (Windows)`、応答は JSON（UTF-8）。時刻は ISO 8601 UTC。
- 認証: `Authorization: Bearer <token>`。ヘッダなし=匿名。**不正/失効トークンは401（匿名にフォールバックしない）**。
- 上限値: `0` = 無制限。

## エラー形式
```json
{ "error": { "code": "SIZE_LIMIT_EXCEEDED", "message": "（日本語で人が読める説明）", "details": { } } }
```
| HTTP | code | details | 意味とクライアントの動作 |
|---|---|---|---|
| 400 | INVALID_REQUEST | — | 入力不正。再試行しない |
| 401 | UNAUTHORIZED | — | トークン無効/失効。再認可を促す（匿名にしない） |
| 403 | FORBIDDEN | — | 権限なし |
| 404 | NOT_FOUND | — | 対象なし/他人のリソース |
| 413 | SIZE_LIMIT_EXCEEDED | `limitBytes`, `kind` | 画像→縮小して再送／動画→エラー表示 |
| 415 | UNSUPPORTED_MEDIA | — | 種別不正（マジックバイト不一致含む）。再試行しない |
| 429 | DAILY_LIMIT_EXCEEDED | `resetsAt` | 1日上限。停止→翌日再開、停止中の写真は送らない |
| 429 | RATE_LIMITED | `retryAfterSeconds`（+`Retry-After`ヘッダ） | 待って再試行 |
| 500/503 | INTERNAL / SERVICE_UNAVAILABLE | — | 指数バックオフで再試行 |

## 認可フロー（ループバック + PKCE）
1. クライアントが `code_verifier`（43〜128文字）、`code_challenge`=BASE64URL(SHA256(verifier))、`state`（ランダム）、`connection_code`（`XXX-XXX`、紛らわしい文字を除いた英数大文字）を生成し、127.0.0.1 のランダムポートで受信待ちを開始する。
2. ブラウザで開く:
   `GET /qick/authorize?client_id=clipshare-quick&redirect_uri=http://127.0.0.1:{port}/callback&state=…&code_challenge=…&code_challenge_method=S256&connection_code=XXX-XXX&device_name=…`
   - `redirect_uri` は `http://127.0.0.1:{port}/…` または `http://[::1]:{port}/…` のみ許可。`code_challenge_method` は `S256` のみ。`device_name` は最大64文字。
3. 未ログインなら既存ログインを経て戻る。認可画面に **`connection_code` を表示**し、クライアント画面の表示と照合させる。「許可」（CSRF対策つきPOST）または「キャンセル」。
4. 許可: `302 {redirect_uri}?code=…&state=…`。キャンセル: `?error=access_denied&state=…`。`code` は使い捨て・10分以内・ハッシュ保存。
5. クライアントが `state` を検証し、トークンを交換する。

### `POST /api/v1/auth/token`
リクエスト（JSON）: `{ "code": "…", "code_verifier": "…", "redirect_uri": "http://127.0.0.1:{port}/callback" }`
200: `{ "token": "…", "tokenId": "…", "tokenType": "Bearer", "user": { "id": "…", "name": "…" }, "limits": <Limits> }`
トークン平文は**この応答でのみ**返す。400（検証失敗・期限切れ・使用済み）。

### `DELETE /api/v1/auth/token`（Bearer）
自分自身のトークンを失効。204。（アンインストール時の連携解除に使う）

### `GET /api/v1/me`（Bearer 任意）
200: `{ "authenticated": true|false, "user": {"id","name"}|null, "limits": <Limits> }`
`Limits` = `{ "imageMaxBytes": n, "videoMaxBytes": n, "dailyLimit": n, "dailyUsed": n, "resetsAt": "ISO" , "banner": true|false }`（`0`=無制限。`dailyUsed` は現行Clipshareの数え方に一致）

## クイック共有
### `POST /api/v1/quick`
`multipart/form-data`: `file`（必須）、`quality`=`original`|`shrink`（既定 `shrink`、画像のみ有効）。
201:
```json
{ "id": "abc123XYZ0", "url": "https://clipshare.link/q/abc123XYZ0", "kind": "IMAGE", "status": "READY",
  "expiresAt": "2026-11-21T00:00:00Z", "deleteToken": "…(匿名のときのみ)" }
```
- 動画は `status: "PROCESSING"` で返ることがある（既存の非同期処理）。URL は即時に共有可能であること（処理中表示は閲覧ページ側）。**この挙動は既存実装を確認し、異なれば本書を更新**。
- バナー: トークンあり=なし／匿名=あり。画像処理: 現行（回転補正）＋ `shrink`=長辺2000px WebP、`original`=高画質WebP再エンコード・メタデータ除去。

### `DELETE /api/v1/quick/{id}`
- Bearer: 所有者のみ。他人/存在しない=404。
- 匿名アップロードは `X-Delete-Token: <deleteToken>` で削除可。
- 204。

## クライアント更新情報
### `GET /api/v1/client/latest`（認証不要・サーバー側でキャッシュ）
200: `{ "version": "1.2.3", "publishedAt": "ISO", "notes": "…", "minSupportedVersion": "1.0.0"|null, "assets": [ { "name": "ClipshareQuick-1.2.3.msix", "url": "https://github.com/…", "sizeBytes": n, "sha256": "…" } ] }`
GitHub API 失敗時は最後に成功した結果を返す。

## 変更履歴
- v1 (2026-11-20): 初版（設計案）。
