<#
.SYNOPSIS
  Clipshare Quick の MSIX を作る（必要なら署名もする）。

.DESCRIPTION
  1. target\release の成果物（clipshare-quick.exe / cq_shell_ext.dll / clipshare-quick-setup.exe）を
     作業フォルダに集める。openvr_api.dll があれば同梱する（-OpenVrDll）。
  2. ロゴ画像（PNG）を生成し、AppxManifest.xml の {{VERSION}} と {{PUBLISHER}} を置き換える。
  3. makeappx.exe で .msix を作る。-PfxPath を指定したときだけ signtool.exe で署名する。
  4. SHA-256 を出力する（client/latest の sha256 と GitHub Releases に使う）。

  Windows SDK（makeappx.exe / signtool.exe）が必要。証明書の秘密鍵はリポジトリに置かないこと。
  署名パスワードは引数ではなく環境変数 CQ_PFX_PASSWORD から読む（ログに残さないため）。

.EXAMPLE
  .\installer\build-msix.ps1 -Version 0.1.0 -Publisher "CN=Clipshare Dev"
  .\installer\build-msix.ps1 -Version 0.1.0 -Publisher "CN=Clipshare Dev" -PfxPath C:\secure\dev.pfx
#>
param(
  [Parameter(Mandatory = $true)][string]$Version,
  [Parameter(Mandatory = $true)][string]$Publisher,
  [string]$PfxPath = "",
  [string]$OpenVrDll = "",
  [string]$OutDir = "dist"
)
$ErrorActionPreference = "Stop"

if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version は x.y.z 形式で指定してください" }
$root = Resolve-Path (Join-Path $PSScriptRoot "..")
$release = Join-Path $root "target\release"
$stage = Join-Path $root "$OutDir\stage"
$assets = Join-Path $stage "Assets"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $assets | Out-Null

foreach ($f in @("clipshare-quick.exe", "cq_shell_ext.dll")) {
  $src = Join-Path $release $f
  if (-not (Test-Path $src)) { throw "$f がありません。先に cargo build --release を実行してください" }
  Copy-Item $src $stage
}
if ($OpenVrDll -ne "") {
  if (-not (Test-Path $OpenVrDll)) { throw "openvr_api.dll が見つかりません: $OpenVrDll" }
  Copy-Item $OpenVrDll (Join-Path $stage "openvr_api.dll")
}

# ロゴ（緑の角丸に C）
Add-Type -AssemblyName System.Drawing
function New-Logo([int]$size, [string]$path) {
  $bmp = New-Object System.Drawing.Bitmap $size, $size
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
  $g.Clear([System.Drawing.Color]::Transparent)
  $r = [int]($size * 0.22)
  $gp = New-Object System.Drawing.Drawing2D.GraphicsPath
  $gp.AddArc(0, 0, 2 * $r, 2 * $r, 180, 90); $gp.AddArc($size - 2 * $r - 1, 0, 2 * $r, 2 * $r, 270, 90)
  $gp.AddArc($size - 2 * $r - 1, $size - 2 * $r - 1, 2 * $r, 2 * $r, 0, 90); $gp.AddArc(0, $size - 2 * $r - 1, 2 * $r, 2 * $r, 90, 90)
  $gp.CloseFigure()
  $g.FillPath((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 0x48, 0xd6, 0xa8))), $gp)
  $font = New-Object System.Drawing.Font("Segoe UI", [float]($size * 0.5), [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
  $fmt = New-Object System.Drawing.StringFormat; $fmt.Alignment = "Center"; $fmt.LineAlignment = "Center"
  $g.DrawString("C", $font, (New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 0x06, 0x11, 0x0d))), (New-Object System.Drawing.RectangleF 0, 0, $size, $size), $fmt)
  $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $bmp.Dispose()
}
New-Logo 44 (Join-Path $assets "Square44x44Logo.png")
New-Logo 150 (Join-Path $assets "Square150x150Logo.png")
New-Logo 50 (Join-Path $assets "StoreLogo.png")

$manifest = Get-Content (Join-Path $PSScriptRoot "AppxManifest.xml") -Raw -Encoding UTF8
$manifest = $manifest.Replace("{{VERSION}}", "$Version.0").Replace("{{PUBLISHER}}", [System.Security.SecurityElement]::Escape($Publisher))
[System.IO.File]::WriteAllText((Join-Path $stage "AppxManifest.xml"), $manifest, (New-Object System.Text.UTF8Encoding($false)))

function Find-SdkTool([string]$name) {
  $cmd = Get-Command $name -ErrorAction SilentlyContinue
  if ($cmd) { return $cmd.Source }
  $kits = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
  $hit = Get-ChildItem $kits -Recurse -Filter $name -ErrorAction SilentlyContinue | Where-Object { $_.FullName -match '\\x64\\' } | Sort-Object FullName -Descending | Select-Object -First 1
  if (-not $hit) { throw "$name が見つかりません（Windows SDK が必要です）" }
  return $hit.FullName
}

$out = Join-Path $root $OutDir
$msix = Join-Path $out "ClipshareQuick-$Version.msix"
& (Find-SdkTool "makeappx.exe") pack /o /d $stage /p $msix | Out-Host
if ($LASTEXITCODE -ne 0) { throw "makeappx に失敗しました" }

if ($PfxPath -ne "") {
  if (-not $env:CQ_PFX_PASSWORD) { throw "環境変数 CQ_PFX_PASSWORD を設定してください" }
  & (Find-SdkTool "signtool.exe") sign /fd SHA256 /a /f $PfxPath /p $env:CQ_PFX_PASSWORD $msix | Out-Host
  if ($LASTEXITCODE -ne 0) { throw "signtool に失敗しました" }
}

$setup = Join-Path $release "clipshare-quick-setup.exe"
if (Test-Path $setup) { Copy-Item $setup (Join-Path $out "ClipshareQuick-Setup-$Version.exe") }

$hash = (Get-FileHash $msix -Algorithm SHA256).Hash.ToLowerInvariant()
$size = (Get-Item $msix).Length
Write-Host "MSIX: $msix"
Write-Host "size: $size"
Write-Host "sha256: $hash"
