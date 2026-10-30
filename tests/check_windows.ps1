param([Parameter(Mandatory = $true)][string]$PackageDirectory)
$ErrorActionPreference = 'Stop'
foreach ($path in @((Join-Path $PackageDirectory 'setup.ps1'),
                    (Join-Path $PSScriptRoot '..\scripts\sign_windows.ps1'), $PSCommandPath)) {
    $tokens = $null
    $errors = $null
    [Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors) | Out-Null
    if ($errors.Count) { throw ($errors | Out-String) }
}
& (Join-Path $PSScriptRoot 'check_windows_state.ps1') -SetupPath (Join-Path $PackageDirectory 'setup.ps1')
$cert = [Security.Cryptography.X509Certificates.X509Certificate2]::new((Join-Path $PackageDirectory 'ltxk.cer'))
if ($cert.HasPrivateKey -or $cert.Subject -ne 'CN=ChenNeuroscience ltxk') { throw 'Unexpected public signing certificate.' }
foreach ($name in @('ltxk.exe', 'ltxk-launch.exe', 'ltxk-shell.dll', 'ltxk.msix')) {
    $signature = Get-AuthenticodeSignature -LiteralPath (Join-Path $PackageDirectory $name)
    if (-not $signature.SignerCertificate -or $signature.SignerCertificate.Thumbprint -ne $cert.Thumbprint -or
        $signature.Status -eq 'HashMismatch') { throw "Incorrect signature on $name." }
}
# Verify the shipped executables expose the custom icon, rather than silently
# falling back to Windows' generic application icon when linking resources.
Add-Type -AssemblyName System.Drawing
foreach ($name in @('ltxk.exe', 'ltxk-launch.exe')) {
    $icon = [Drawing.Icon]::ExtractAssociatedIcon((Join-Path $PackageDirectory $name))
    if (-not $icon) { throw "Missing application icon in $name." }
    $bitmap = $null
    try {
        $bitmap = $icon.ToBitmap()
        $red = $false; $green = $false; $blue = $false
        for ($y = 0; $y -lt $bitmap.Height; $y++) {
            for ($x = 0; $x -lt $bitmap.Width; $x++) {
                $pixel = $bitmap.GetPixel($x, $y)
                if ($pixel.A -lt 128) { continue }
                if ($pixel.R -gt $pixel.G + 40 -and $pixel.R -gt $pixel.B + 40) { $red = $true }
                if ($pixel.G -gt $pixel.R + 40 -and $pixel.G -gt $pixel.B + 40) { $green = $true }
                if ($pixel.B -gt $pixel.R + 40 -and $pixel.B -gt $pixel.G + 40) { $blue = $true }
            }
        }
        if (-not ($red -and $green -and $blue)) { throw "The cluster icon is missing from $name." }
    } finally {
        if ($bitmap) { $bitmap.Dispose() }
        $icon.Dispose()
    }
}
# Windows PowerShell 5.1 ignores -Include with -LiteralPath. Filter the
# enumerated files explicitly so ordinary package files are not false positives.
if (Get-ChildItem -LiteralPath $PackageDirectory -Recurse -File |
    Where-Object { $_.Extension -in @('.pfx', '.key', '.lib', '.exp', '.obj') }) {
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
$ns.AddNamespace('desktop6', 'http://schemas.microsoft.com/appx/manifest/desktop/windows10/6')
$ns.AddNamespace('rescap', 'http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities')
if ($manifest.SelectSingleNode('//uap10:AllowExternalContent', $ns).InnerText -ne 'true' -or
    $manifest.SelectSingleNode('//desktop5:Verb', $ns).Clsid -ne 'B0EA0498-10EE-49F3-A631-3613B698965C') {
    throw 'Missing sparse identity or Explorer command registration.'
}
# Check both copies: sharing permission alone does not disable AppData writes
# being redirected into a private location invisible to the unpackaged CLI.
foreach ($document in @($manifest, $external)) {
    $policy = $document.SelectSingleNode('/*[local-name()="Package"]/*[local-name()="Properties"]/desktop6:FileSystemWriteVirtualization', $ns)
    if (-not $policy -or $policy.InnerText -ne 'disabled' -or
        -not $document.SelectSingleNode('//rescap:Capability[@Name="unvirtualizedResources"]', $ns)) {
        throw 'The Explorer package must disable AppData file virtualization so selection files and the queue are shared with ltxk.exe.'
    }
}
$embedded = [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes((Join-Path $PackageDirectory 'ltxk-launch.exe')))
foreach ($text in @('urn:schemas-microsoft-com:msix.v1', 'ChenNeuroscience.ltxk', 'CN=ChenNeuroscience ltxk')) {
    if (-not $embedded.Contains($text)) { throw "Missing embedded launcher identity: $text" }
}
Write-Host 'Windows PowerShell syntax, signed package, external identity, and public-certificate checks passed.'
