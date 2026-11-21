<#
.SYNOPSIS
  release-checksums.ps1 の自動テスト（CI とローカルで実行する。ネットワーク・証明書は使わない）。
#>
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$script = Join-Path $PSScriptRoot "release-checksums.ps1"
$v = "1.2.3"
$exe = "ClipshareQuick-Setup-$v.exe"
$msix = "ClipshareQuick-$v.msix"
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("cq-sums-" + [guid]::NewGuid().ToString("N"))
$utf8 = New-Object System.Text.UTF8Encoding($false)
$failures = 0

function New-Case([string]$Name) {
  $d = Join-Path $work $Name
  New-Item -ItemType Directory -Force $d | Out-Null
  [System.IO.File]::WriteAllBytes((Join-Path $d $exe), [byte[]](1, 2, 3, 4))
  [System.IO.File]::WriteAllBytes((Join-Path $d $msix), [byte[]](5, 6, 7))
  & $script -Mode Write -Dir $d -Version $v *> $null
  return $d
}

function Get-Hex([string]$Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }

function Get-Digests([string]$Dir) {
  $o = [ordered]@{}
  foreach ($n in @($exe, $msix, "SHA256SUMS")) { $o[$n] = "sha256:" + (Get-Hex (Join-Path $Dir $n)) }
  return ($o | ConvertTo-Json -Compress)
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

try {
  # 正常系: 形式（行・順序・空白 2 つ・LF・BOM なし）
  Assert-Ok "作成した内容が sha256sum 互換の形式" {
    $d = New-Case "format"
    $bytes = [System.IO.File]::ReadAllBytes((Join-Path $d "SHA256SUMS"))
    $want = (Get-Hex (Join-Path $d $exe)) + "  $exe`n" + (Get-Hex (Join-Path $d $msix)) + "  $msix`n"
    if ($utf8.GetString($bytes) -cne $want) { throw "内容が違う" }
    if ($bytes[0] -eq 0xEF) { throw "BOM がある" }
  }
  Assert-Ok "検証が通る（digest なし）" { $d = New-Case "ok"; & $script -Mode Verify -Dir $d -Version $v }
  Assert-Ok "検証が通る（digest あり）" {
    $d = New-Case "ok-digest"; & $script -Mode Verify -Dir $d -Version $v -DigestsJson (Get-Digests $d)
  }

  # 作成時の欠落
  Assert-Fails "作成: MSIX が無い" "見つかりません" {
    $d = Join-Path $work "w-missing"; New-Item -ItemType Directory -Force $d | Out-Null
    [System.IO.File]::WriteAllBytes((Join-Path $d $exe), [byte[]](1))
    & $script -Mode Write -Dir $d -Version $v
  }
  Assert-Fails "作成: 空のファイル" "空のファイル" {
    $d = Join-Path $work "w-empty"; New-Item -ItemType Directory -Force $d | Out-Null
    [System.IO.File]::WriteAllBytes((Join-Path $d $exe), [byte[]](1))
    [System.IO.File]::WriteAllBytes((Join-Path $d $msix), [byte[]]@())
    & $script -Mode Write -Dir $d -Version $v
  }
  Assert-Fails "バージョンの形式" "X.Y.Z" { & $script -Mode Write -Dir $work -Version "1.2" }

  # 検証時の不一致・欠落・余分
  Assert-Fails "改ざん（MSIX）" "一致しません: $msix" {
    $d = New-Case "tamper"; [System.IO.File]::WriteAllBytes((Join-Path $d $msix), [byte[]](9, 9))
    & $script -Mode Verify -Dir $d -Version $v
  }
  Assert-Fails "添付の欠落（セットアップ exe）" "添付ファイルがありません: $exe" {
    $d = New-Case "missing"; Remove-Item (Join-Path $d $exe); & $script -Mode Verify -Dir $d -Version $v
  }
  Assert-Fails "SHA256SUMS の欠落" "添付ファイルがありません: SHA256SUMS" {
    $d = New-Case "missing-sums"; Remove-Item (Join-Path $d "SHA256SUMS"); & $script -Mode Verify -Dir $d -Version $v
  }
  Assert-Fails "余分な添付（.appinstaller）" "想定外の添付ファイル" {
    $d = New-Case "extra"; [System.IO.File]::WriteAllText((Join-Path $d "ClipshareQuick.appinstaller"), "x")
    & $script -Mode Verify -Dir $d -Version $v
  }
  Assert-Fails "SHA256SUMS に余分な行" "行数" {
    $d = New-Case "extra-line"; $p = Join-Path $d "SHA256SUMS"
    $t = $utf8.GetString([System.IO.File]::ReadAllBytes($p)) + ("0" * 64) + "  ClipshareQuick.appinstaller`n"
    [System.IO.File]::WriteAllText($p, $t, $utf8); & $script -Mode Verify -Dir $d -Version $v
  }
  Assert-Fails "CRLF" "LF" {
    $d = New-Case "crlf"; $p = Join-Path $d "SHA256SUMS"
    $t = $utf8.GetString([System.IO.File]::ReadAllBytes($p)).Replace("`n", "`r`n")
    [System.IO.File]::WriteAllText($p, $t, $utf8); & $script -Mode Verify -Dir $d -Version $v
  }
  Assert-Fails "BOM" "BOM" {
    $d = New-Case "bom"; $p = Join-Path $d "SHA256SUMS"
    $t = $utf8.GetString([System.IO.File]::ReadAllBytes($p))
    [System.IO.File]::WriteAllText($p, $t, (New-Object System.Text.UTF8Encoding($true))); & $script -Mode Verify -Dir $d -Version $v
  }
  Assert-Fails "大文字の hex" "形式" {
    $d = New-Case "upper"; $p = Join-Path $d "SHA256SUMS"
    $t = $utf8.GetString([System.IO.File]::ReadAllBytes($p))
    $t = $t.Substring(0, 64).ToUpperInvariant() + $t.Substring(64)
    [System.IO.File]::WriteAllText($p, $t, $utf8); & $script -Mode Verify -Dir $d -Version $v
  }
  Assert-Fails "空白 1 つ" "形式" {
    $d = New-Case "space"; $p = Join-Path $d "SHA256SUMS"
    $t = $utf8.GetString([System.IO.File]::ReadAllBytes($p)).Replace("  ", " ")
    [System.IO.File]::WriteAllText($p, $t, $utf8); & $script -Mode Verify -Dir $d -Version $v
  }
  Assert-Fails "順序の入れ替え" "ファイル名が違います" {
    $d = New-Case "order"; $p = Join-Path $d "SHA256SUMS"
    $l = $utf8.GetString([System.IO.File]::ReadAllBytes($p)).TrimEnd("`n").Split("`n")
    [System.IO.File]::WriteAllText($p, $l[1] + "`n" + $l[0] + "`n", $utf8); & $script -Mode Verify -Dir $d -Version $v
  }
  Assert-Fails "ファイル名の大文字小文字" "添付ファイルがありません: $msix" {
    $d = New-Case "case"; Rename-Item (Join-Path $d $msix) ($msix.ToLowerInvariant())
    & $script -Mode Verify -Dir $d -Version $v
  }
  Assert-Fails "digest の不一致" "digest が一致しません" {
    $d = New-Case "digest-bad"
    $j = (Get-Digests $d) | ConvertFrom-Json; $j.$msix = "sha256:" + ("0" * 64)
    & $script -Mode Verify -Dir $d -Version $v -DigestsJson ($j | ConvertTo-Json -Compress)
  }
  Assert-Fails "digest が null" "digest がありません" {
    $d = New-Case "digest-null"
    $j = (Get-Digests $d) | ConvertFrom-Json; $j.$exe = $null
    & $script -Mode Verify -Dir $d -Version $v -DigestsJson ($j | ConvertTo-Json -Compress)
  }
  Assert-Fails "digest が無い（SHA256SUMS）" "digest がありません: SHA256SUMS" {
    $d = New-Case "digest-missing"
    $j = (Get-Digests $d) | ConvertFrom-Json; $j.PSObject.Properties.Remove("SHA256SUMS")
    & $script -Mode Verify -Dir $d -Version $v -DigestsJson ($j | ConvertTo-Json -Compress)
  }
}
finally {
  Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

if ($failures -gt 0) { throw "release-checksums.ps1 のテストに $failures 件失敗しました" }
Write-Host "release-checksums.ps1 のテストはすべて成功しました"
