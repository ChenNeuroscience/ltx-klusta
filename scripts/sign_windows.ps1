param([Parameter(Mandatory = $true)][string]$PackageDirectory)
$ErrorActionPreference = 'Stop'
$certificate = $null
$temporaryPfx = $null
$preexisting = @(Get-ChildItem Cert:\CurrentUser\My | ForEach-Object { $_.Thumbprint })
try {
    if ($env:LTXK_SIGNING_PFX_BASE64) {
        # A persistent self-signed publisher key can be kept in Actions secrets.
        # It is never copied into the downloadable package or printed to logs.
        $temporaryPfx = Join-Path ([IO.Path]::GetTempPath()) (([guid]::NewGuid().ToString()) + '.pfx')
        [IO.File]::WriteAllBytes($temporaryPfx, [Convert]::FromBase64String($env:LTXK_SIGNING_PFX_BASE64))
        $password = ConvertTo-SecureString $env:LTXK_SIGNING_PFX_PASSWORD -AsPlainText -Force
        $certificate = Import-PfxCertificate -FilePath $temporaryPfx -Password $password -CertStoreLocation Cert:\CurrentUser\My
    } else {
        # Every build is installable without configuring a paid service or secret.
        # Without a persistent key, setup must trust the new certificate per build.
        $certificate = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=ChenNeuroscience ltxk' `
            -KeyAlgorithm RSA -KeyLength 2048 -HashAlgorithm SHA256 -KeyExportPolicy NonExportable `
            -NotAfter (Get-Date).AddYears(5) -CertStoreLocation Cert:\CurrentUser\My
    }
    if ($certificate.Subject -ne 'CN=ChenNeuroscience ltxk' -or -not $certificate.HasPrivateKey) {
        throw 'The signing certificate must have subject CN=ChenNeuroscience ltxk and a private key.'
    }
    foreach ($name in @('ltxk.exe', 'ltxk-launch.exe', 'ltxk-shell.dll', 'ltxk.msix')) {
        & signtool.exe sign /fd SHA256 /sha1 $certificate.Thumbprint /s My (Join-Path $PackageDirectory $name)
        if ($LASTEXITCODE -ne 0) { throw "Signing $name failed." }
    }
    Export-Certificate -Cert $certificate -FilePath (Join-Path $PackageDirectory 'ltxk.cer') | Out-Null
} finally {
    if ($temporaryPfx) { Remove-Item -LiteralPath $temporaryPfx -Force -ErrorAction SilentlyContinue }
    if ($certificate -and $certificate.Thumbprint -notin $preexisting) {
        Remove-Item -LiteralPath ("Cert:\CurrentUser\My\" + $certificate.Thumbprint) -DeleteKey -Force -ErrorAction SilentlyContinue
    }
}
