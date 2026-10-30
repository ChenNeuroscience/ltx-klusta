# Windows Explorer integration

The preferred Windows install is **Install Explorer integration.cmd** from the
extracted release zip. It installs a private copy under
`%LOCALAPPDATA%\Programs\ltxk`, adds **Run ltxk** to Explorer, and creates an
**ltx-klusta** shortcut in Start. The zip remains usable as a portable app.

## Certificate trust

Setup shows the public signing certificate's publisher and SHA-1 thumbprint,
then asks Windows for administrator approval to import that exact certificate
into **Local Computer → Trusted People**. It does not import a root CA
certificate or install a private signing key. Package registration runs as the
original Windows user, even if another administrator supplies the UAC credentials.
The certificate thumbprint identifies the certificate; package signatures and
file checksums use SHA-256.

Self-signing has no account, payment, or Microsoft review requirement. Each
computer must trust the certificate before installing the signed identity
package. Microsoft's [MSIX trust instructions](https://learn.microsoft.com/en-us/windows/msix/app-installer/troubleshoot-appinstaller-issues#trusted-certificates)
describe the Trusted People store. This trust step does not establish Microsoft
SmartScreen download reputation; a downloaded installer or executable can still
show a warning. Managed PCs can have policies requiring IT to install the app.

To update, close the old ltxk console, extract the new zip, and run its installer.
Setup preserves `ltxk.json` from the previous installation unless the new zip
already contains your own `ltxk.json`. A new publisher certificate requires a
new trust approval. The installer uses separate directories for versions so a
DLL still loaded by Explorer does not block copying the update.

**Uninstall Explorer integration.cmd** removes the current user's registration
and Start shortcut. It keeps the queue, results, settings, and certificate
trust so reinstalling remains possible. From PowerShell in the extracted or
installed package folder, this also removes certificates that setup added:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\setup.ps1 -Action Uninstall -RemoveCertificate
```

Certificate removal asks for administrator approval and refuses to remove trust
while another Windows user still has ltxk registered. Certificates that were
already trusted before setup are not removed. Settings and queue data are kept.

## Behaviour

The native COM DLL implements `IExplorerCommand`, registered through a sparse
MSIX identity package. The manifest registers the command for files generally;
`GetState` inspects the entire selection and hides the verb unless every item
is a filesystem file with a numeric final extension. It excludes `.clu.N`,
`.fet.N`, `.res.N`, and `.spk.N` sidecars, case-insensitively. This adds an
operation without taking ownership of numeric file associations or double-click
behaviour. See Microsoft's [Explorer integration guide](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/integrate-packaged-app-with-file-explorer).

`Invoke` writes one UTF-8 JSON selection in the user's queue `requests` folder
and launches `ltxk-launch.exe`, a GUI executable. The helper runs the hidden
`ltxk enqueue --request FILE` command without creating a console. The Rust
command validates the complete batch before submitting it through the normal
queue admission lock. The helper removes its request on completion or failure;
submission errors are shown in a dialog and `%LOCALAPPDATA%\ltxk\queue\launcher.log`.
Neither selected paths nor install paths are evaluated by a command shell.

The package explicitly disables AppData file-write virtualization alongside
the `unvirtualizedResources` capability. The packaged COM host and the portable
Rust executable must see the same selection files and queue. Permission to use
unvirtualized resources alone does not disable file redirection; a request
written into a private package location can otherwise be invisible to the
process reading it. See Microsoft's [AppData virtualization settings](https://learn.microsoft.com/en-us/windows/msix/desktop/flexible-virtualization).

The helper serializes concurrent launches using `desktop-launch.lock`. It
checks the visible console's OS lease and verifies its PID plus process creation
time before attaching briefly to locate the actual host window. It restores
that window and selects the matching Windows Terminal tab through UI Automation.
If no visible session exists it starts `ltxk.exe` with `CREATE_NEW_CONSOLE`, which
uses the user's configured default host, including Windows Terminal. This does
not depend on `wt.exe` being on PATH. Windows can decline foreground activation;
the helper then flashes the taskbar button. Hosts exposing no local console
window, such as remote or some embedded terminals, require switching manually;
the helper reports this instead of opening duplicate dashboards. Custom tab
titles or ambiguous/inaccessible tabs can also require selecting the tab manually.

Interactive consoles retain their lease while idle and resume when new jobs
arrive. Redirected CLI output does not occupy the visible-console lease, so an
Explorer dashboard can also follow a background or redirected coordinator.
No resident service is installed. OS locks release on exit or crash; stale owner
metadata alone cannot prevent a fresh console from opening.

## Building and signing

`scripts/package_rust.py --platform windows` invokes `scripts/build_windows.py`.
Build on Windows x64 with the Visual Studio C++ tools and Windows SDK installed
(the `windows-2022` CI image supplies these). The helper and DLL use the static
C++ runtime, matching the Rust build's static runtime. Packaging builds and runs
the COM/selection/quoting smoke test, creates the sparse MSIX package, and signs
the executables, DLL, and MSIX with `scripts/sign_windows.ps1`. Only the public
`ltxk.cer` is distributed. Native build intermediates and private keys are excluded.

The Rust build script embeds `assets/ltxk.ico` in `ltxk.exe`; packaging embeds
the same icon in the GUI helper and copies the exported MSIX logos. The source
tree keeps the SVG, platform exports and regeneration instructions in `assets/`.

By default CI creates a fresh self-signed certificate for each build. This
needs no configuration, but every new release requires a new trust approval.
For trust that persists across releases, create one private publisher key on a
trusted Windows machine and configure these GitHub Actions repository secrets:

- `LTXK_SIGNING_PFX_BASE64`: base64 of the password-protected PFX.
- `LTXK_SIGNING_PFX_PASSWORD`: its password.

The certificate must use the subject `CN=ChenNeuroscience ltxk` and permit code
signing. For example, in PowerShell, outside the source tree:

```powershell
$cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=ChenNeuroscience ltxk' -KeyAlgorithm RSA -KeyLength 2048 -HashAlgorithm SHA256 -KeyExportPolicy Exportable -NotAfter (Get-Date).AddYears(5) -CertStoreLocation Cert:\CurrentUser\My
$password = Read-Host 'Choose a signing-key password' -AsSecureString
Export-PfxCertificate -Cert $cert -FilePath .\ltxk-signing.pfx -Password $password
```

Store the PFX and password securely; do not commit either or include them in
releases. Encode the PFX locally and place the result in the repository secret.
The build imports the private key temporarily, signs, then removes its newly
imported certificate/key and temporary PFX. A certificate already present in
the build user's store is retained. Fork pull requests use the automatic self-signed fallback
because repository secrets are unavailable to them. The package identity stays
the same across signing-key changes, but clients must trust the replacement key.
Certificate expiry/replacement requires a newly valid signing certificate and
another trust approval; installed apps continue running after certificate expiry.

## Validation

Automated checks cover filename eligibility and quoting, real Shell selection
arrays, COM object lifetime, Unicode batch submission, duplicate and invalid
selections, and an idle dashboard accepting a later batch. Windows package tests
parse the PowerShell scripts, inspect the sparse manifest and embedded identity,
and verify that all signatures match the public certificate. They do not add
certificate trust or install Explorer integration on the build runner.

Before releasing, verify on a Windows 11 desktop:

1. Extract the zip into a folder containing spaces and Unicode. Run setup as
   a standard user; approve certificate trust with an administrator account.
   Confirm the menu and Start shortcut belong to the standard user.
2. Select `.1` and `.2` recordings together. **Run ltxk** must appear directly in
   the modern menu, process both, and write `.clu.N` results beside them. The
   command must be hidden for a sidecar, folder, or mixed unsupported selection.
3. Submit another batch while running, while minimized, and after completion.
   Check the existing window/tab is restored and no duplicate dashboard appears.
   Repeat with a console first started from the CLI and with two quick invocations.
4. Exercise Windows Terminal (including another selected tab) and classic Console
   Host. Close/kill a console and check the next invocation recovers unfinished
   work. Confirm Enter closes an idle console and a later selection opens a new one.
5. Reinstall the same release, upgrade, and uninstall. Check settings survive,
   the menu/shortcut are removed, and certificate removal respects other users.
