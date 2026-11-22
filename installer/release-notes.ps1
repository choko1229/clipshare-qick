<#
.SYNOPSIS
  CHANGELOG.md の「## [X.Y.Z] - YYYY-MM-DD」の節を取り出し、GitHub Release の本文にする。

.DESCRIPTION
  Release の本文は Clipshare のダウンロードページ（/qick/download）の「更新履歴」に出る。
  Clipshare は本文の箇条書きを 1 項目 140 文字まで表示するため（lib/quick-client/release-notes.ts）、
  それを超える項目があれば失敗する（切り詰めて表示されるのを防ぐ）。
    - 節が無い、日付の無い見出し（未確定）、箇条書きが 1 つも無い → 失敗
    - 出力は UTF-8（BOM なし）、改行は LF

.EXAMPLE
  .\installer\release-notes.ps1 -Version 0.1.0 -Out dist\RELEASE_NOTES.md
#>
param(
  [Parameter(Mandatory = $true)][string]$Version,
  [Parameter(Mandatory = $true)][string]$Out,
  [string]$Changelog = ""
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

# 既定はリポジトリ直下の CHANGELOG.md（Windows PowerShell 5.1 では param の既定値の中で $PSScriptRoot が空になるため、ここで決める）
if ($Changelog -eq "") { $Changelog = Join-Path (Split-Path -Parent $PSScriptRoot) "CHANGELOG.md" }

# Clipshare 側の表示上限（RELEASE_NOTE_ITEM_MAX_CHARS）と合わせる
$MaxItemChars = 140

if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "バージョンは X.Y.Z 形式にしてください: $Version" }
if (-not (Test-Path -LiteralPath $Changelog -PathType Leaf)) { throw "CHANGELOG.md が見つかりません: $Changelog" }

$strict = New-Object System.Text.UTF8Encoding($false, $true)
try { $text = $strict.GetString([System.IO.File]::ReadAllBytes($Changelog)) } catch { throw "CHANGELOG.md が UTF-8 ではありません" }
$text = $text.TrimStart([char]0xFEFF)
$lines = $text -split "\r?\n"

$heading = '^## \[' + [regex]::Escape($Version) + '\] - \d{4}-\d{2}-\d{2}\s*$'
$start = -1
for ($i = 0; $i -lt $lines.Count; $i++) {
  if ($lines[$i] -match $heading) {
    if ($start -ge 0) { throw "CHANGELOG.md に [$Version] の節が 2 つあります" }
    $start = $i
  }
}
if ($start -lt 0) { throw "CHANGELOG.md に「## [$Version] - YYYY-MM-DD」の節がありません" }

$body = New-Object System.Collections.Generic.List[string]
for ($i = $start + 1; $i -lt $lines.Count; $i++) {
  if ($lines[$i] -match '^## ') { break }
  $body.Add($lines[$i].TrimEnd())
}
while ($body.Count -gt 0 -and $body[0] -eq "") { $body.RemoveAt(0) }
while ($body.Count -gt 0 -and $body[$body.Count - 1] -eq "") { $body.RemoveAt($body.Count - 1) }

$items = 0
foreach ($line in $body) {
  if ($line -match '^\s{0,3}[-*+]\s+(.+)$') {
    $items++
    $len = (New-Object System.Globalization.StringInfo($Matches[1].Trim())).LengthInTextElements
    if ($len -gt $MaxItemChars) { throw "$MaxItemChars 文字を超える項目があります（$len 文字）: $($Matches[1].Trim())" }
  }
}
if ($items -eq 0) { throw "CHANGELOG.md の [$Version] の節に箇条書きがありません" }

$outText = ($body -join "`n") + "`n"
$outPath = [System.IO.Path]::GetFullPath($Out)
$dir = Split-Path -Parent $outPath
if (-not (Test-Path -LiteralPath $dir -PathType Container)) { New-Item -ItemType Directory -Force $dir | Out-Null }
[System.IO.File]::WriteAllText($outPath, $outText, (New-Object System.Text.UTF8Encoding($false)))
Write-Host "Release の本文（[$Version]、$items 項目）:"
Write-Host -NoNewline $outText
