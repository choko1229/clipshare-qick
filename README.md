# Clipshare Quick

VRChat で写真を撮ると自動で [Clipshare](https://clipshare.link) にアップロードし、共有URLをクリップボードへ入れる Windows ツールです。
エクスプローラーの右クリックからも画像・動画の共有URLを発行できます。SteamVR では視界内の通知とダッシュボードパネルが使えます。

> 開発中のバージョンです。配布は Clipshare のダウンロードページ（`/qick/download`）から行う予定です。

## できること

- **自動アップロード**: VRChat の写真フォルダ（年月のサブフォルダを含む）に保存された新しい写真を送り、共有URLをコピーします。起動前からある写真は送りません。
- **右クリック共有**: 画像・動画を選んで「Clipshareで共有」。複数選択すると URL を改行区切りでコピーします。画像は「原寸」「縮小（長辺2000px）」を選べます。
- **履歴**: トレイアイコンから直近50件。URL の再コピー、削除、失敗した送信の再試行。期限（24時間）切れも表示します。
- **ログインせずに使う**: 画像にバナーが付き、1日5件までです。ログインするとバナーなし・上限が上がります。
- **SteamVR**: アップロードの状況を視界内に表示。ダッシュボードから自動アップロードの切り替えと直近の写真の削除ができます。

## 動作環境

- Windows 11（右クリックメニューは Windows 11 のみ）
- SteamVR 連携は任意（SteamVR が無くても動きます）

## プライバシーとセキュリティ

- 自動アップロードは、初回セットアップで同意したとき（または設定で ON にしたとき）だけ動きます。
- 写真の撮影情報などの埋め込みデータは、アップロード時に取り除かれます。保存期間は24時間です。
- Clipshare との連携トークンは Windows の資格情報マネージャーにだけ保存します。ログにトークンやファイルの内容は書きません。
- 連携が切れた（トークンが無効になった）場合は、匿名に切り替えずにアップロードを止め、再ログインを求めます。

## 開発

構成（Cargo ワークスペース）:

| クレート | 内容 |
|---|---|
| `crates/core` | 設定・ログ・API クライアント・ブラウザ認可・トークン保管・履歴・フォルダ監視・アップロード・更新判定（UI なし） |
| `crates/ipc` | 本体と右クリック拡張の間の名前付きパイプ（同じ利用者だけが接続できる） |
| `crates/app` | トレイ常駐アプリ（egui）、通知、更新の適用、セットアップ（`clipshare-quick-setup`） |
| `crates/shell-ext` | 右クリック拡張（`IExplorerCommand` の COM DLL） |
| `crates/vr` | SteamVR（OpenVR）の HUD 通知とダッシュボードパネル |
| `crates/mock-server` | API 契約 v1 のモックサーバー（開発・結合テスト用） |
| `installer/` | MSIX のマニフェストと作成・署名の手順 |

仕様・設計は `docs/quickshare/`（`SPEC.md`・`API_CONTRACT.md`・`INSTRUCTIONS.md`）を参照してください。

### ビルド

```powershell
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

- CI（GitHub Actions）は MSVC ツールチェーンでビルドします。
- MSVC が無い環境では、GNU ツールチェーン（`x86_64-pc-windows-gnu`）と MinGW-w64（binutils を含むもの。例: WinLibs）でもビルドできます。C コンパイラを必要とする依存はありません（TLS は Windows 標準の schannel）。

### 本番に接続せずに動かす

```powershell
cargo run -p cq-mock-server -- 8787
```

別のフォルダを `LOCALAPPDATA` に指定し、その中の `ClipshareQuick\settings.json` に `{"advanced":{"apiBaseUrl":"http://127.0.0.1:8787"}}` を書いてから起動します。
`CQ_DEV_NO_SYSTEM_CHANGES=1` を付けると、スタートアップ登録や通知の登録（レジストリ）を変更しません。

```powershell
$env:LOCALAPPDATA = "C:\temp\cq-dev"; $env:CQ_DEV_NO_SYSTEM_CHANGES = "1"; cargo run -p clipshare-quick
```

### パッケージとリリース

`installer/README.md` を参照してください（署名・タグ・GitHub Releases の公開は人が行います）。

## ライセンス

MIT（`LICENSE`）
