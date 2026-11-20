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

## 4. テスト結果

（最終実行の結果を下に記載）

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

QUESTIONS.md の Q-001〜Q-020（状態はすべて「未確認」）。
