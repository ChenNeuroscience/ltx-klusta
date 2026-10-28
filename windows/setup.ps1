[CmdletBinding()]
param(
    [ValidateSet('Install', 'Uninstall', 'Trust', 'RemoveTrust')][string]$Action = 'Install',
    [string]$CertificatePath,
    [string]$ExpectedThumbprint,
    [switch]$RemoveCertificate
)
$ErrorActionPreference = 'Stop'
$packageName = 'ChenNeuroscience.ltxk'
$publisher = 'CN=ChenNeuroscience ltxk'
$installRoot = Join-Path $env:LOCALAPPDATA 'Programs\ltxk'
$statePath = Join-Path $installRoot 'installation.json'
$shortcutPath = Join-Path ([Environment]::GetFolderPath('Programs')) 'ltx-klusta.lnk'
$powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'

function Invoke-TrustAction([string]$TrustAction, [string]$Path, [string]$Thumbprint) {
    # Elevate only certificate maintenance. Package registration and shortcuts
    # remain in the original user's process, even if UAC asks for another account.
    # Values are single-quoted PS literals inside an encoded command, never code.
    if ($TrustAction -notin @('Trust', 'RemoveTrust') -or $Thumbprint -notmatch '^[0-9A-Fa-f]{40}$') {
        throw 'Invalid certificate maintenance request.'
    }
    $scriptLiteral = $PSCommandPath.Replace("'", "''")
    $pathLiteral = $Path.Replace("'", "''")
    $command = "& '$scriptLiteral' -Action $TrustAction -CertificatePath '$pathLiteral' -ExpectedThumbprint '$Thumbprint'"
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
    $child = Start-Process -FilePath $powershell -Verb RunAs -Wait -PassThru `
        -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', $encoded)
    if ($child.ExitCode -ne 0) { throw "Certificate maintenance failed (exit $($child.ExitCode))." }
}

function Read-State {
    if (Test-Path -LiteralPath $statePath) {
        return Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json
    }
    return $null
}

function Save-State([string]$ActiveDirectory, [object[]]$ManagedCertificates) {
    $temporary = Join-Path $installRoot ([guid]::NewGuid().ToString('N') + '.json')
    try {
        @{ ActiveDirectory = $ActiveDirectory; ManagedCertificates = @($ManagedCertificates | Where-Object { $_ } | Select-Object -Unique) } |
            ConvertTo-Json | Set-Content -LiteralPath $temporary -Encoding UTF8
        if (Test-Path -LiteralPath $statePath) { [IO.File]::Replace($temporary, $statePath, $null) }
        else { [IO.File]::Move($temporary, $statePath) }
    } finally {
        Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
    }
}

function Install-Integration {
    if ([Environment]::OSVersion.Version.Build -lt 19041) { throw 'Explorer integration needs Windows 10 version 2004 or newer. The modern menu needs Windows 11.' }
    if (-not [Environment]::Is64BitOperatingSystem) { throw 'This package needs 64-bit Windows.' }
    $nativeArchitecture = $env:PROCESSOR_ARCHITECTURE
    if ($env:PROCESSOR_ARCHITEW6432) { $nativeArchitecture = $env:PROCESSOR_ARCHITEW6432 }
    if ($nativeArchitecture -ne 'AMD64') { throw 'Explorer integration currently needs Intel/AMD x64 Windows.' }
    foreach ($name in @('ltxk.exe', 'ltxk-launch.exe', 'ltxk-shell.dll', 'ltxk.msix', 'ltxk.cer', 'AppxManifest.xml', 'SHA256SUMS.txt')) {
        if (-not (Test-Path -LiteralPath (Join-Path $PSScriptRoot $name))) { throw "Missing $name. Extract the complete Windows zip first." }
    }
    foreach ($line in Get-Content -LiteralPath (Join-Path $PSScriptRoot 'SHA256SUMS.txt')) {
        if ($line -notmatch '^([0-9a-fA-F]{64})  (.+)$') { throw 'Invalid package checksum list.' }
        $expected = $Matches[1]
        $name = $Matches[2]
        $sourceRoot = [IO.Path]::GetFullPath($PSScriptRoot).TrimEnd('\') + '\'
        $fullName = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot $name))
        if ([IO.Path]::IsPathRooted($name) -or -not $fullName.StartsWith($sourceRoot, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Invalid filename in checksum list.'
        }
        if ((Get-FileHash -LiteralPath (Join-Path $PSScriptRoot $name) -Algorithm SHA256).Hash -ne $expected) {
            throw "Checksum mismatch for $name. Download and extract the package again."
        }
    }
    $certPath = Join-Path $PSScriptRoot 'ltxk.cer'
    $cert = [Security.Cryptography.X509Certificates.X509Certificate2]::new($certPath)
    if ($cert.Subject -ne $publisher -or $cert.HasPrivateKey -or $cert.NotAfter -le (Get-Date)) {
        throw 'The bundled public signing certificate is invalid or expired.'
    }
    $signature = Get-AuthenticodeSignature -LiteralPath (Join-Path $PSScriptRoot 'ltxk.msix')
    if (-not $signature.SignerCertificate -or $signature.SignerCertificate.Thumbprint -ne $cert.Thumbprint -or
        $signature.Status -eq 'HashMismatch') {
        throw 'The Explorer package was not signed by the bundled certificate.'
    }
    [xml]$manifest = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'AppxManifest.xml') -Raw
    if ($manifest.Package.Identity.Name -ne $packageName -or $manifest.Package.Identity.Publisher -ne $publisher) {
        throw 'Unexpected Explorer package identity.'
    }
    $version = [version]$manifest.Package.Identity.Version
    $oldState = Read-State
    $existing = Get-AppxPackage -Name $packageName
    if ($existing -and $existing.Publisher -ne $publisher) { throw 'Another publisher has registered this package name.' }
    if ($existing -and [version]$existing.Version -gt $version) { throw 'A newer ltxk version is installed. Use the latest Windows download.' }
    $ownedCertificates = @()
    if ($oldState) { $ownedCertificates = @($oldState.ManagedCertificates) }
    $trustedPath = 'Cert:\LocalMachine\TrustedPeople\' + $cert.Thumbprint
    if (-not (Test-Path -LiteralPath $trustedPath)) {
        Write-Host 'Windows will ask an administrator to trust this ltxk signing certificate.'
        Write-Host ('Publisher: ' + $cert.Subject)
        Write-Host ('Certificate thumbprint: ' + $cert.Thumbprint)
        Invoke-TrustAction 'Trust' $certPath $cert.Thumbprint
        if (-not (Test-Path -LiteralPath $trustedPath)) { throw 'The certificate was not added to Trusted People.' }
        $ownedCertificates += $cert.Thumbprint
    }
    # Keep a certificate ledger even if copying or registration later fails.
    # A failed first install can then remove its newly added trust on uninstall.
    $previousDirectory = ''
    if ($oldState) { $previousDirectory = $oldState.ActiveDirectory }
    Save-State $previousDirectory $ownedCertificates
    # Versioned directories avoid replacing a DLL still loaded by COM/Explorer.
    $target = Join-Path $installRoot ('app-' + $version + '-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $target | Out-Null
    Get-ChildItem -LiteralPath $PSScriptRoot | Copy-Item -Destination $target -Recurse
    if ($oldState -and $oldState.ActiveDirectory -and -not (Test-Path -LiteralPath (Join-Path $target 'ltxk.json'))) {
        $oldConfig = Join-Path $oldState.ActiveDirectory 'ltxk.json'
        if (Test-Path -LiteralPath $oldConfig) { Copy-Item -LiteralPath $oldConfig -Destination $target }
    }
    $removedExisting = $false
    try {
        try {
            Add-AppxPackage -Path (Join-Path $target 'ltxk.msix') -ExternalLocation $target -ErrorAction Stop
        } catch {
            # Same-version relocation is the only registration failure that
            # permits removing a working registration before retrying.
            $code = '{0:X8}' -f ($_.Exception.HResult -band 0xffffffffL)
            if ($code -ne '80073CFB' -or -not $existing -or [version]$existing.Version -ne $version -or
                -not $oldState -or -not $oldState.ActiveDirectory -or
                -not (Test-Path -LiteralPath (Join-Path $oldState.ActiveDirectory 'ltxk.msix'))) { throw }
            Remove-AppxPackage -Package $existing.PackageFullName -ErrorAction Stop
            $removedExisting = $true
            Add-AppxPackage -Path (Join-Path $target 'ltxk.msix') -ExternalLocation $target -ErrorAction Stop
        }
    } catch {
        if ($removedExisting -and $oldState -and $oldState.ActiveDirectory) {
            Add-AppxPackage -Path (Join-Path $oldState.ActiveDirectory 'ltxk.msix') -ExternalLocation $oldState.ActiveDirectory -ErrorAction Continue
        }
        Remove-Item -LiteralPath $target -Recurse -Force -ErrorAction SilentlyContinue
        throw
    }
    Save-State $target $ownedCertificates
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($shortcutPath)
    $shortcut.TargetPath = Join-Path $target 'ltxk-launch.exe'
    $shortcut.WorkingDirectory = $target
    $shortcut.Description = 'Open the ltx-klusta queue'
    $shortcut.Save()
    # Notify the shell without interrupting any open Explorer windows.
    Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class LtxkShell { [DllImport("shell32.dll")] public static extern void SHChangeNotify(uint e, uint f, IntPtr a, IntPtr b); }'
    [LtxkShell]::SHChangeNotify(0x08000000, 0, [IntPtr]::Zero, [IntPtr]::Zero)
    Write-Host ''
    Write-Host 'Installed. Select recording files such as session.1 and session.2, right-click, and choose Run ltxk.'
    Write-Host 'Open ltx-klusta from Start to see the queue again.'
    Write-Host 'If the menu has not appeared, restart File Explorer or sign out and back in.'
    Write-Host ('Settings: ' + (Join-Path $target 'ltxk.json'))
}

function Uninstall-Integration {
    $state = Read-State
    Get-AppxPackage -Name $packageName | Where-Object { $_.Publisher -eq $publisher } | Remove-AppxPackage -ErrorAction Stop
    Remove-Item -LiteralPath $shortcutPath -Force -ErrorAction SilentlyContinue
    if ($state) {
        if ($RemoveCertificate) {
            foreach ($thumbprint in @($state.ManagedCertificates)) {
                Invoke-TrustAction 'RemoveTrust' '' $thumbprint
            }
        }
        # Keep configuration and the queue. Loaded extension files can be
        # removed later; failure to delete them must not restore registration.
        foreach ($folder in Get-ChildItem -LiteralPath $installRoot -Directory -Filter 'app-*') {
            if ($folder.Name -notmatch '^app-\d+\.\d+\.\d+\.\d+-[0-9a-f]{32}$' -or
                ($folder.Attributes -band [IO.FileAttributes]::ReparsePoint)) { continue }
            if (Test-Path -LiteralPath (Join-Path $folder.FullName 'ltxk.json')) { continue }
            Remove-Item -LiteralPath $folder.FullName -Recurse -Force -ErrorAction SilentlyContinue
        }
        if ($RemoveCertificate) { Remove-Item -LiteralPath $statePath -Force }
    }
    Write-Host 'Explorer integration and the Start shortcut were removed. Your queue, results, and settings are kept.'
    if (-not $RemoveCertificate) { Write-Host 'Certificate trust is kept for reinstalling. Use setup.ps1 -Action Uninstall -RemoveCertificate to remove certificates added by setup.' }
}

$setupLock = $null
try {
    if ($Action -in @('Install', 'Uninstall')) {
        New-Item -ItemType Directory -Path $installRoot -Force | Out-Null
        try {
            $setupLock = [IO.File]::Open((Join-Path $installRoot 'setup.lock'), [IO.FileMode]::OpenOrCreate,
                                        [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        } catch { throw 'Another ltxk setup is running, or the install folder is not writable.' }
    }
    switch ($Action) {
        'Trust' {
            if ($ExpectedThumbprint -notmatch '^[0-9A-Fa-f]{40}$') { throw 'Invalid certificate thumbprint.' }
            $cert = [Security.Cryptography.X509Certificates.X509Certificate2]::new($CertificatePath)
            if ($cert.Thumbprint -ne $ExpectedThumbprint -or $cert.Subject -ne $publisher -or $cert.HasPrivateKey) {
                throw 'The certificate does not match the requested ltxk publisher.'
            }
            Import-Certificate -FilePath $CertificatePath -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null
        }
        'RemoveTrust' {
            if ($ExpectedThumbprint -notmatch '^[0-9A-Fa-f]{40}$') { throw 'Invalid certificate thumbprint.' }
            # Certificate trust is shared by Windows users; keep it if anyone
            # still has an ltxk package registered on this computer.
            if (Get-AppxPackage -AllUsers -Name $packageName) { throw 'Another Windows user still has ltxk installed; certificate trust was kept.' }
            $path = 'Cert:\LocalMachine\TrustedPeople\' + $ExpectedThumbprint
            if (Test-Path -LiteralPath $path) {
                if ((Get-Item -LiteralPath $path).Subject -ne $publisher) { throw 'This is not an ltxk certificate.' }
                Remove-Item -LiteralPath $path -Force
            }
        }
        'Install' { Install-Integration }
        'Uninstall' { Uninstall-Integration }
    }
} catch {
    Write-Error $_
    exit 1
} finally {
    if ($setupLock) { $setupLock.Dispose() }
}
