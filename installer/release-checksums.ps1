<#
.SYNOPSIS
  リリースの SHA256SUMS を作る（Write）／ダウンロードした添付ファイルと照合する（Verify）。

.DESCRIPTION
  形式（sha256sum 互換。QUESTIONS Q-028）:
    - 1 行 1 ファイル。`<小文字の 64 桁 hex><空白 2 つ><ファイル名>`
    - 対象はセットアップ exe と MSIX の 2 ファイルだけ（この順）。.appinstaller などは含めない。
    - ファイル名は GitHub Releases の添付ファイル名と完全に一致させる。
    - UTF-8（BOM なし）、改行は LF、最終行も LF で終える。

  -Mode Write  : -Dir の 2 ファイルから -Dir\SHA256SUMS を作る。どちらかが無い・空なら失敗する。
  -Mode Verify : -Dir に「2 ファイル + SHA256SUMS」だけがあること、SHA256SUMS の形式と内容が
                 実ファイルの SHA-256 と一致することを確認する。-DigestsJson を渡すと、GitHub が添付ファイルに
                 付ける digest（"sha256:<hex>"）とも照合する。どれか 1 つでも違えば失敗する（例外）。

.EXAMPLE
  .\installer\release-checksums.ps1 -Mode Write -Dir dist -Version 0.1.0
  .\installer\release-checksums.ps1 -Mode Verify -Dir verify -Version 0.1.0 -DigestsJson '{"SHA256SUMS":"sha256:..."}'
#>
param(
  [Parameter(Mandatory = $true)][ValidateSet("Write", "Verify")][string]$Mode,
  [Parameter(Mandatory = $true)][string]$Dir,
  [Parameter(Mandatory = $true)][string]$Version,
  [string]$DigestsJson = ""
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "バージョンは X.Y.Z 形式にしてください: $Version" }

$SumsName = "SHA256SUMS"
# 順序も固定する（Q-022: セットアップ exe が主、MSIX は従）
$AssetNames = @("ClipshareQuick-Setup-$Version.exe", "ClipshareQuick-$Version.msix")

function Get-Sha256Hex([string]$Path) {
  return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function New-SumsText([string]$Folder) {
  $sb = New-Object System.Text.StringBuilder
  foreach ($n in $AssetNames) {
    $p = Join-Path $Folder $n
    if (-not (Test-Path -LiteralPath $p -PathType Leaf)) { throw "SHA256SUMS の対象が見つかりません: $n" }
    if ((Get-Item -LiteralPath $p).Length -le 0) { throw "SHA256SUMS の対象が空のファイルです: $n" }
    [void]$sb.Append((Get-Sha256Hex $p)).Append("  ").Append($n).Append("`n")
  }
  return $sb.ToString()
}

$folder = [System.IO.Path]::GetFullPath($Dir)
if (-not (Test-Path -LiteralPath $folder -PathType Container)) { throw "フォルダが見つかりません: $folder" }
$sumsPath = Join-Path $folder $SumsName

if ($Mode -eq "Write") {
  $text = New-SumsText $folder
  [System.IO.File]::WriteAllText($sumsPath, $text, (New-Object System.Text.UTF8Encoding($false)))
  Write-Host "SHA256SUMS:"
  Write-Host -NoNewline $text
  return
}

# ---- Verify ----
# 1. 添付ファイルの集合がちょうど「2 ファイル + SHA256SUMS」であること（欠落・余分を検出）
$expected = @($AssetNames + $SumsName)
$actual = @(Get-ChildItem -LiteralPath $folder -Force | ForEach-Object { $_.Name })
foreach ($n in $expected) {
  if ($actual -cnotcontains $n) { throw "添付ファイルがありません: $n" }
}
foreach ($n in $actual) {
  if ($expected -cnotcontains $n) { throw "想定外の添付ファイルがあります: $n" }
}

# 2. SHA256SUMS の形式（BOM なし・LF・2 行・行の形式・ファイル名と順序）
$bytes = [System.IO.File]::ReadAllBytes($sumsPath)
if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
  throw "SHA256SUMS に BOM があります"
}
$strict = New-Object System.Text.UTF8Encoding($false, $true)
try { $text = $strict.GetString($bytes) } catch { throw "SHA256SUMS が UTF-8 ではありません" }
if ($text.Contains("`r")) { throw "SHA256SUMS の改行が LF ではありません" }
if (-not $text.EndsWith("`n")) { throw "SHA256SUMS の最終行が改行で終わっていません" }
$lines = $text.Substring(0, $text.Length - 1).Split("`n")
if ($lines.Length -ne $AssetNames.Length) {
  throw "SHA256SUMS の行数が $($AssetNames.Length) ではありません: $($lines.Length)"
}
for ($i = 0; $i -lt $lines.Length; $i++) {
  $m = [regex]::Match($lines[$i], '^([0-9a-f]{64})  (.+)$')
  if (-not $m.Success) { throw "SHA256SUMS の $($i + 1) 行目の形式が違います" }
  $name = $m.Groups[2].Value
  if ($name -cne $AssetNames[$i]) { throw "SHA256SUMS の $($i + 1) 行目のファイル名が違います: $name（期待: $($AssetNames[$i])）" }
  # 3. 実ファイルの SHA-256 と一致すること
  $hash = Get-Sha256Hex (Join-Path $folder $name)
  if ($m.Groups[1].Value -cne $hash) { throw "SHA-256 が一致しません: $name" }
}
# 念のため、作り直した内容とバイト単位で一致すること
if ($text -cne (New-SumsText $folder)) { throw "SHA256SUMS の内容が実ファイルから作った内容と一致しません" }

# 4. GitHub の digest（Clipshare 側はこれと SHA256SUMS の一致を配布の条件にする。Q-021）
if ($DigestsJson -ne "") {
  $digests = $DigestsJson | ConvertFrom-Json
  foreach ($n in $expected) {
    $prop = $digests.PSObject.Properties[$n]
    if ($null -eq $prop -or $null -eq $prop.Value -or "$($prop.Value)" -eq "") { throw "GitHub の digest がありません: $n" }
    $d = "$($prop.Value)"
    if ($d -cnotmatch '^sha256:[0-9a-f]{64}$') { throw "GitHub の digest の形式が違います: $n" }
    if ($d.Substring(7) -cne (Get-Sha256Hex (Join-Path $folder $n))) { throw "GitHub の digest が一致しません: $n" }
  }
}

Write-Host "SHA256SUMS を確認しました（$($AssetNames -join ', ')）"
