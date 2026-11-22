<#
.SYNOPSIS
  release-notes.ps1 の自動テスト（CI とローカルで実行する。ネットワークは使わない）。
#>
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$script = Join-Path $PSScriptRoot "release-notes.ps1"
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("cq-notes-" + [guid]::NewGuid().ToString("N"))
$utf8 = New-Object System.Text.UTF8Encoding($false)
$failures = 0

function New-Changelog([string]$Name, [string]$Text) {
  $d = Join-Path $work $Name
  New-Item -ItemType Directory -Force $d | Out-Null
  $p = Join-Path $d "CHANGELOG.md"
  [System.IO.File]::WriteAllText($p, $Text, $utf8)
  return $p
}

function Assert-Ok([string]$Title, [scriptblock]$Body) {
  try { & $Body *> $null; Write-Host "ok   $Title" }
  catch { $script:failures++; Write-Host "FAIL $Title : $($_.Exception.Message)" }
}

function Assert-Fails([string]$Title, [string]$Expect, [scriptblock]$Body) {
  try { & $Body *> $null; $script:failures++; Write-Host "FAIL $Title : 失敗しなかった" }
  catch {
    if ($_.Exception.Message -like "*$Expect*") { Write-Host "ok   $Title" }
    else { $script:failures++; Write-Host "FAIL $Title : 想定と違う失敗: $($_.Exception.Message)" }
  }
}

$sample = @"
# 変更履歴

## [Unreleased]

### 追加
- 未リリースの項目

## [1.2.3] - 2026-11-22

### 追加
- 新しい項目 A
- 新しい項目 B

### 修正
- 直した項目

## [1.2.2] - 2026-11-01

### 追加
- 古い項目
"@ -replace "`r`n", "`n"

try {
  Assert-Ok "指定したバージョンの節だけを取り出す（LF・BOM なし）" {
    $c = New-Changelog "ok" $sample
    $o = Join-Path $work "ok\out.md"
    & $script -Version 1.2.3 -Changelog $c -Out $o
    $bytes = [System.IO.File]::ReadAllBytes($o)
    if ($bytes[0] -eq 0xEF) { throw "BOM がある" }
    $t = $utf8.GetString($bytes)
    if ($t.Contains("`r")) { throw "CR がある" }
    $want = "### 追加`n- 新しい項目 A`n- 新しい項目 B`n`n### 修正`n- 直した項目`n"
    if ($t -cne $want) { throw "内容が違う: [$t]" }
  }
  Assert-Ok "CRLF の CHANGELOG でも読める" {
    $c = New-Changelog "crlf" ($sample -replace "`n", "`r`n")
    $o = Join-Path $work "crlf\out.md"
    & $script -Version 1.2.3 -Changelog $c -Out $o
    if (([System.IO.File]::ReadAllText($o)).Contains("`r")) { throw "CR がある" }
  }
  Assert-Ok "最後の節も取り出せる" {
    $c = New-Changelog "last" $sample
    $o = Join-Path $work "last\out.md"
    & $script -Version 1.2.2 -Changelog $c -Out $o
    if ([System.IO.File]::ReadAllText($o) -cne "### 追加`n- 古い項目`n") { throw "内容が違う" }
  }
  Assert-Fails "節が無い" "の節がありません" {
    $c = New-Changelog "missing" $sample
    & $script -Version 9.9.9 -Changelog $c -Out (Join-Path $work "missing\out.md")
  }
  Assert-Fails "日付の無い見出し（未確定）" "の節がありません" {
    $c = New-Changelog "nodate" ($sample -replace "## \[1\.2\.3\] - 2026-11-22", "## [1.2.3]")
    & $script -Version 1.2.3 -Changelog $c -Out (Join-Path $work "nodate\out.md")
  }
  Assert-Fails "同じバージョンの節が 2 つ" "2 つあります" {
    $c = New-Changelog "dup" ($sample + "`n## [1.2.3] - 2026-11-23`n- 重複`n")
    & $script -Version 1.2.3 -Changelog $c -Out (Join-Path $work "dup\out.md")
  }
  Assert-Fails "箇条書きが無い" "箇条書きがありません" {
    $c = New-Changelog "empty" "# 変更履歴`n`n## [1.2.3] - 2026-11-22`n`n説明だけ`n"
    & $script -Version 1.2.3 -Changelog $c -Out (Join-Path $work "empty\out.md")
  }
  Assert-Ok "140 文字ちょうどは通る" {
    $c = New-Changelog "max" ("## [1.2.3] - 2026-11-22`n- " + ("あ" * 140) + "`n")
    & $script -Version 1.2.3 -Changelog $c -Out (Join-Path $work "max\out.md")
  }
  Assert-Fails "141 文字は失敗" "140 文字を超える項目があります（141 文字）" {
    $c = New-Changelog "over" ("## [1.2.3] - 2026-11-22`n- " + ("あ" * 141) + "`n")
    & $script -Version 1.2.3 -Changelog $c -Out (Join-Path $work "over\out.md")
  }
  Assert-Fails "-Changelog を省くとリポジトリの CHANGELOG.md を読む" "9.9.9] - YYYY-MM-DD」の節がありません" {
    & $script -Version 9.9.9 -Out (Join-Path $work "default\out.md")
  }
  Assert-Fails "バージョンの形式" "X.Y.Z 形式" {
    $c = New-Changelog "badver" $sample
    & $script -Version "v1.2.3" -Changelog $c -Out (Join-Path $work "badver\out.md")
  }
}
finally {
  Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

if ($failures -gt 0) { throw "release-notes.ps1 のテストに $failures 件失敗しました" }
Write-Host "release-notes.ps1 のテストはすべて成功しました"
