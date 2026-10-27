param([Parameter(Mandatory = $true)][string]$PackageDirectory)
$ErrorActionPreference = 'Stop'
foreach ($path in @((Join-Path $PackageDirectory 'setup.ps1'),
                    (Join-Path $PSScriptRoot '..\scripts\sign_windows.ps1'), $PSCommandPath)) {
    $tokens = $null
    $errors = $null
    [Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors) | Out-Null
    if ($errors.Count) { throw ($errors | Out-String) }
}
$cert = [Security.Cryptography.X509Certificates.X509Certificate2]::new((Join-Path $PackageDirectory 'ltxk.cer'))
if ($cert.HasPrivateKey -or $cert.Subject -ne 'CN=ChenNeuroscience ltxk') { throw 'Unexpected public signing certificate.' }
foreach ($name in @('ltxk.exe', 'ltxk-launch.exe', 'ltxk-shell.dll', 'ltxk.msix')) {
    $signature = Get-AuthenticodeSignature -LiteralPath (Join-Path $PackageDirectory $name)
    if (-not $signature.SignerCertificate -or $signature.SignerCertificate.Thumbprint -ne $cert.Thumbprint -or
        $signature.Status -eq 'HashMismatch') { throw "Incorrect signature on $name." }
}
if (Get-ChildItem -LiteralPath $PackageDirectory -Recurse -Include '*.pfx', '*.key', '*.lib', '*.exp', '*.obj') {
    throw 'Private keys or native build intermediates were packaged.'
}
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [IO.Compression.ZipFile]::OpenRead((Join-Path $PackageDirectory 'ltxk.msix'))
try {
    if (-not $zip.GetEntry('AppxSignature.p7x')) { throw 'The MSIX has no signature.' }
    $reader = [IO.StreamReader]::new($zip.GetEntry('AppxManifest.xml').Open())
    try { [xml]$manifest = $reader.ReadToEnd() } finally { $reader.Dispose() }
} finally { $zip.Dispose() }
[xml]$external = Get-Content -LiteralPath (Join-Path $PackageDirectory 'AppxManifest.xml') -Raw
foreach ($name in @('Name', 'Publisher', 'Version', 'ProcessorArchitecture')) {
    if ($manifest.Package.Identity.$name -ne $external.Package.Identity.$name) { throw "Sparse manifest mismatch: $name" }
}
if ($manifest.Package.Identity.Publisher -ne $cert.Subject -or $manifest.Package.Identity.Name -ne 'ChenNeuroscience.ltxk') {
    throw 'The certificate and package identity do not match.'
}
$ns = [Xml.XmlNamespaceManager]::new($manifest.NameTable)
$ns.AddNamespace('uap10', 'http://schemas.microsoft.com/appx/manifest/uap/windows10/10')
$ns.AddNamespace('desktop5', 'http://schemas.microsoft.com/appx/manifest/desktop/windows10/5')
if ($manifest.SelectSingleNode('//uap10:AllowExternalContent', $ns).InnerText -ne 'true' -or
    $manifest.SelectSingleNode('//desktop5:Verb', $ns).Clsid -ne 'B0EA0498-10EE-49F3-A631-3613B698965C') {
    throw 'Missing sparse identity or Explorer command registration.'
}
$embedded = [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes((Join-Path $PackageDirectory 'ltxk-launch.exe')))
foreach ($text in @('urn:schemas-microsoft-com:msix.v1', 'ChenNeuroscience.ltxk', 'CN=ChenNeuroscience ltxk')) {
    if (-not $embedded.Contains($text)) { throw "Missing embedded launcher identity: $text" }
}
Write-Host 'Windows PowerShell syntax, signed package, external identity, and public-certificate checks passed.'
