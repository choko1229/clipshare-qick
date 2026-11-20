# インストーラー（MSIX）と署名・リリースの手順

> この手順の実行（証明書の作成、署名、タグの作成、GitHub Releases の公開、実機へのインストール）は
> **人が行う**。自走の作業では手順書の作成までにとどめている（CLAUDE.md の「止まってよい操作」）。

## 構成

| ファイル | 内容 |
|---|---|
| `AppxManifest.xml` | MSIX のマニフェスト（右クリック拡張・実行エイリアス・スタートアップ・書き込み仮想化の無効化） |
| `build-msix.ps1` | release ビルドの成果物から `.msix` を作り、必要なら署名し、SHA-256 を出す |
| `new-dev-certificate.ps1` | 自己署名のコード署名証明書を作る（秘密鍵はリポジトリの外へ出力） |

配布物（GitHub Releases に添付するもの）:

- `ClipshareQuick-x.y.z.msix`（署名済み）
- `ClipshareQuick-Setup-x.y.z.exe`（画面 02・03 のセットアップ。同じフォルダの `.msix` をインストールする）
- `ClipshareQuick.cer`（自己署名の間だけ。公開鍵のみ）

## 1. 準備（初回のみ）

1. Windows SDK（`makeappx.exe` / `signtool.exe`）を入れる。
2. 自己署名の証明書を作る（**公開時に正式な証明書へ切り替えるかは要判断**: QUESTIONS Q-006）。
   ```powershell
   .\installer\new-dev-certificate.ps1 -Subject "CN=Clipshare Dev" -OutDir C:\secure
   ```
   - `C:\secure\ClipshareQuick-dev.pfx` は秘密に保管する（リポジトリ・チャット・ログに出さない）。
   - Subject は `-Publisher` に渡す値と完全に一致させる。

## 2. ビルドとパッケージ作成

```powershell
cargo build --workspace --release --locked
$env:CQ_PFX_PASSWORD = Read-Host "PFX のパスワード"   # 画面に表示されない入力にしたい場合は SecureString を使う
.\installer\build-msix.ps1 -Version 0.1.0 -Publisher "CN=Clipshare Dev" -PfxPath C:\secure\ClipshareQuick-dev.pfx -OpenVrDll C:\path\to\openvr_api.dll
Remove-Item Env:\CQ_PFX_PASSWORD
```

- `openvr_api.dll` は Valve の OpenVR SDK（BSD-3-Clause）の `bin/win64/openvr_api.dll`。同梱しない場合、SteamVR 連携は無効になる（アプリ本体は動作する）。
- 出力された `sha256` と `size` を Clipshare 側の `GET /api/v1/client/latest` の `assets[]` に設定する（更新の検証に使う）。

## 3. 開発機でのインストール確認（隔離手順）

自己署名の証明書をテスト用 PC（または仮想マシン・Windows サンドボックス）だけで信頼させる。普段使いの PC の信頼ストアを変更しない。

```powershell
# 管理者の PowerShell（テスト用 PC で）
Import-Certificate -FilePath .\ClipshareQuick.cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople
Add-AppxPackage .\dist\ClipshareQuick-0.1.0.msix
# 確認が終わったら
Get-AppxPackage choko1229.ClipshareQuick | Remove-AppxPackage
Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object Subject -eq "CN=Clipshare Dev" | Remove-Item
```

確認項目は `docs/quickshare/MANUAL_TESTS.md` を参照。

## 4. リリース（タグ → ビルド → 署名 → GitHub Releases）

`.github/workflows/release.yml` が `v*` タグの push で動き、ビルド・パッケージ作成・署名を行い、
**下書き（draft）の Release** に成果物を添付する。公開（Publish release）は人が内容を確認してから行う。

1. `CHANGELOG.md` の `[Unreleased]` をバージョン見出しにする。`Cargo.toml` の `version` を上げる。
2. GitHub のリポジトリ設定 → Secrets に登録する（値はチャットやログに出さない）:
   - `MSIX_PFX_BASE64`: `.pfx` を Base64 にしたもの（`[Convert]::ToBase64String([IO.File]::ReadAllBytes("C:\secure\ClipshareQuick-dev.pfx"))`）
   - `MSIX_PFX_PASSWORD`: `.pfx` のパスワード
   - `MSIX_PUBLISHER`: 例 `CN=Clipshare Dev`
3. `main` にマージ後、タグを作って push する:
   ```powershell
   git tag v0.1.0
   git push origin v0.1.0
   ```
4. Actions の完了後、Releases の下書きを開き、`sha256` を確認して公開する。
5. Clipshare 側の `client/latest`（GitHub Releases から取得・キャッシュ）が新しい版を返すことを確認する。

## 更新の仕組み（アプリ側）

- 起動時と 24 時間ごとに `GET /api/v1/client/latest` を確認し、新しい版なら通知する（勝手に適用しない）。
- 「ダウンロードして更新」→ ダウンロード → **SHA-256・サイズ・Authenticode 署名を検証** → 不一致なら適用しない（画面 21 ⑭）。
- 「今すぐ再起動して更新」: アップロード中のファイルがあれば完了を待ってから、`PackageManager` で適用する。
  右クリック拡張がエクスプローラーに読み込まれていて置き換えられない場合（`ERROR_PACKAGES_IN_USE`）は、次回（再起動後）の適用に切り替える。
- 「次回起動時に更新」: 使用中なら登録を延期するオプションで適用を予約する。
- `.appinstaller` による自動更新は使わない（GitHub Releases の URL で成立するか未検証のため。INSTRUCTIONS §5）。
