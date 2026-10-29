param([string]$SetupPath = (Join-Path $PSScriptRoot '..\windows\setup.ps1'))
$ErrorActionPreference = 'Stop'

# Load only the persistence functions from the actual installer. Do not run
# setup, change certificate trust, register packages, or touch the real profile.
$tokens = $null
$errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($SetupPath, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw ($errors | Out-String) }
$functions = @($ast.FindAll({
    param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -in @('Read-State', 'Save-State')
}, $false))
if ($functions.Count -ne 2) { throw 'Cannot find installer state persistence functions.' }
foreach ($definition in $functions) { . ([scriptblock]::Create($definition.Extent.Text)) }

# Include spaces, literal wildcard characters, and Unicode in the profile path.
# Construct Unicode explicitly so the test also works in Windows PowerShell 5.1
# when the source checkout has no UTF-8 BOM.
$root = Join-Path ([IO.Path]::GetTempPath()) ('ltxk state [literal] & ' + [char]0x03bb + ' ' + [guid]::NewGuid().ToString('N'))
$installRoot = Join-Path $root 'Programs\ltxk'
$statePath = Join-Path $installRoot 'installation.json'
[IO.Directory]::CreateDirectory($installRoot) | Out-Null
try {
    if ($null -ne (Read-State)) { throw 'A fresh profile unexpectedly has installation state.' }
    Save-State '' @()
    $state = Read-State
    if ($state.ActiveDirectory -ne '' -or @($state.ManagedCertificates).Count -ne 0) {
        throw 'Initial certificate ledger was not saved correctly.'
    }

    # First install writes the ledger, then replaces it after registration.
    # Reinstallation and upgrades replace that same state file again.
    $first = Join-Path $installRoot 'app-first'
    $later = Join-Path $installRoot 'app-later'
    $one = 'A' * 40
    $two = 'B' * 40
    $sentinel = Join-Path $installRoot 'keep.txt'
    [IO.File]::WriteAllText($sentinel, 'existing files are kept')
    Save-State $first @($one, $one, $null)
    $state = Read-State
    if ($state.ActiveDirectory -ne $first -or @($state.ManagedCertificates).Count -ne 1 -or
        $state.ManagedCertificates -notcontains $one) { throw 'First installation state was not replaced correctly.' }
    Save-State $later @($one, $two, $one)
    $state = Read-State
    if ($state.ActiveDirectory -ne $later -or @($state.ManagedCertificates).Count -ne 2 -or
        $state.ManagedCertificates -notcontains $one -or $state.ManagedCertificates -notcontains $two) {
        throw 'Upgrade lost the active directory or certificate ownership ledger.'
    }

    # A Windows sharing violation must preserve the prior state and clean up
    # the temporary write, rather than deleting the old file before replacing it.
    if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {
        $previous = [IO.File]::ReadAllText($statePath)
        $locked = [IO.File]::Open($statePath, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        $failed = $false
        try {
            try { Save-State $first @($one) } catch { $failed = $true }
        } finally { $locked.Dispose() }
        if (-not $failed -or [IO.File]::ReadAllText($statePath) -ne $previous) {
            throw 'Failed replacement did not preserve the previous state.'
        }
    }
    if ([IO.File]::ReadAllText($sentinel) -ne 'existing files are kept' -or
        @(Get-ChildItem -LiteralPath $installRoot -File).Count -ne 2) {
        throw 'State persistence changed unrelated files or leaked a temporary file.'
    }
    Write-Host 'Installer initial save, repeated replacement, certificate ledger, and cleanup checks passed.'
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force
}
