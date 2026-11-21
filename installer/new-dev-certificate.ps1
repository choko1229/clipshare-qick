<#
.SYNOPSIS
  開発・暫定配布用の自己署名のコード署名証明書を作る（手順書つき。自動では実行しない）。

.DESCRIPTION
  - 証明書は CurrentUser\My に作られる。秘密鍵つきの .pfx は指定したフォルダに出力する（リポジトリの外に置くこと）。
  - .pfx のパスワードは Read-Host で入力する（コマンドラインやログに残さない）。
  - 公開用の .cer（公開鍵のみ）も出力する。インストール先の PC では、この .cer を
    「ローカル コンピューター\信頼されたユーザー（TrustedPeople）」に入れると MSIX をインストールできる。
  - Subject は AppxManifest.xml の Publisher と完全に一致させること。

.EXAMPLE
  .\installer\new-dev-certificate.ps1 -OutDir C:\secure
  （Subject は installer\signing.json の publisher を使う。-Subject で上書きできる）
#>
param(
  [string]$Subject = "",
  [Parameter(Mandatory = $true)][string]$OutDir
)
$ErrorActionPreference = "Stop"
if ($Subject -eq "") {
  $Subject = (Get-Content (Join-Path $PSScriptRoot "signing.json") -Raw -Encoding UTF8 | ConvertFrom-Json).publisher
}
$repo = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$out = [System.IO.Path]::GetFullPath($OutDir)
if ($out.StartsWith($repo, [System.StringComparison]::OrdinalIgnoreCase)) {
  throw "秘密鍵をリポジトリの中に出力しないでください: $out"
}
New-Item -ItemType Directory -Force $out | Out-Null
$cert = New-SelfSignedCertificate -Type Custom -Subject $Subject -KeyUsage DigitalSignature `
  -FriendlyName "Clipshare Quick (self-signed)" -CertStoreLocation "Cert:\CurrentUser\My" `
  -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}") -NotAfter (Get-Date).AddYears(2)
$pwd = Read-Host -AsSecureString "PFX のパスワード"
Export-PfxCertificate -Cert $cert -FilePath (Join-Path $out "ClipshareQuick-dev.pfx") -Password $pwd | Out-Null
Export-Certificate -Cert $cert -FilePath (Join-Path $out "ClipshareQuick.cer") | Out-Null
Write-Host "拇印: $($cert.Thumbprint)"
Write-Host "出力先: $out（.pfx は秘密に保管し、.cer を配布物に同梱する）"
