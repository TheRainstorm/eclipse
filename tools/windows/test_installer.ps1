# Exercise only generated package files in a newly owned temporary directory.
param([string]$Installer = (Join-Path $PSScriptRoot 'install.ps1'))
$ErrorActionPreference = 'Stop'
$Installer = (Get-Item -LiteralPath $Installer).FullName
$root = Join-Path $env:TEMP ('uu-installer-test-' + [Guid]::NewGuid().ToString('N'))
$baseFiles = @('uu-gui.exe','uu.exe','libgcc_s_seh-1.dll','libstdc++-6.dll','README.txt','install.ps1',
               'LICENSE','LICENSE-MIT','LICENSE-APACHE','THIRD-PARTY-NOTICES.txt')
$target = Join-Path $root 'installed'
$archive = Join-Path $root 'archive'
$utf8 = New-Object System.Text.UTF8Encoding($false)
function New-TestPackage([string]$Name, [int]$Format = 5) {
    $dir = Join-Path $root $Name
    New-Item -ItemType Directory -Path $dir | Out-Null
    $files = @($baseFiles)
    if ($Format -eq 5) { $files[0]='eclipse.exe'; $files[1]='eclipse-cli.exe' }
    if ($Format -ge 2) { $files += 'libvpl.dll' }
    if ($Format -ge 3) { $files += 'libpyrowave-shared-1.dll' }
    if ($Format -ge 4) { $files += @('uu_plugin_sdk.dll','std-0123456789abcdef.dll') }
    $hashes = @{}
    foreach ($file in $files) {
        if ($file -eq 'install.ps1') { Copy-Item -LiteralPath $Installer -Destination (Join-Path $dir $file) }
        else { [IO.File]::WriteAllText((Join-Path $dir $file), "$Name/$file", $utf8) }
        $hashes[$file] = (Get-FileHash -LiteralPath (Join-Path $dir $file)).Hash.ToLowerInvariant()
    }
    $manifest = @{source_commit=('a' * 40);sha256=$hashes}
    if ($Format -ne 1) { $manifest.format_version = $Format }
    if ($Format -ge 4) { $manifest.plugin_runtime = @('uu_plugin_sdk.dll','std-0123456789abcdef.dll'); $manifest.plugin_sdk_id = 'f' * 64 }
    [IO.File]::WriteAllText((Join-Path $dir 'version.json'), ($manifest | ConvertTo-Json -Depth 4), $utf8)
    return $dir
}
function Assert-Test([bool]$Condition, [string]$Message) {
    if (!$Condition) { throw $Message }
}
function Get-TestHashes {
    $hashes = @{}
    foreach ($file in Get-ChildItem -LiteralPath $target -File) { $hashes[$file.Name] = (Get-FileHash -LiteralPath $file.FullName).Hash }
    return $hashes
}
function Assert-TestHashes($Expected) {
    $actual = Get-TestHashes
    Assert-Test ($actual.Count -eq $Expected.Count) 'Installed file count changed'
    foreach ($name in $Expected.Keys) { Assert-Test ($actual[$name] -eq $Expected[$name]) "Installed file changed: $name" }
}
try {
    New-Item -ItemType Directory -Path $root | Out-Null
    $old = New-TestPackage 'old'
    $new = New-TestPackage 'new'
    & $Installer -SourceDir $old -InstallDir $target -ArchiveRoot $archive -NoShortcut | Out-Null
    [IO.File]::WriteAllText((Join-Path $target 'user-config.json'), 'preserved user settings', $utf8)
    $before = Get-TestHashes
    # Denying delete on the last DLL forces a failure after earlier EXE/DLL replacements.
    $locked = [IO.File]::Open((Join-Path $target 'libpyrowave-shared-1.dll'), [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        $failed = $false
        try { & $Installer -SourceDir $new -InstallDir $target -ArchiveRoot $archive -NoShortcut | Out-Null }
        catch { $failed = $true; Assert-Test ($_.Exception.Message -like '*package files were restored*') 'Installer did not report rollback' }
        Assert-Test $failed 'A sharing violation should fail installation'
        Assert-TestHashes $before
    } finally { $locked.Dispose() }
    & $Installer -SourceDir $new -InstallDir $target -ArchiveRoot $archive -NoShortcut | Out-Null
    foreach ($file in @('eclipse.exe','eclipse-cli.exe') + @($baseFiles[2..($baseFiles.Count-1)]) + @('libvpl.dll','libpyrowave-shared-1.dll', 'version.json')) {
        Assert-Test ((Get-FileHash -LiteralPath (Join-Path $target $file)).Hash -eq (Get-FileHash -LiteralPath (Join-Path $new $file)).Hash) "New payload mismatch: $file"
    }
    Assert-Test ([IO.File]::ReadAllText((Join-Path $target 'user-config.json')) -eq 'preserved user settings') 'User settings changed'
    # Exercise a legal non-canonical input; the installer returns full paths.
    $defaultTarget = Join-Path $root 'installed/../default-archive-installed'
    & $Installer -SourceDir $old -InstallDir $defaultTarget -NoShortcut | Out-Null
    $installed = (& $Installer -SourceDir $new -InstallDir $defaultTarget -NoShortcut) | ConvertFrom-Json
    $expectedArchiveRoot = [IO.Path]::GetFullPath((Join-Path $defaultTarget '.upgrade-archive'))
    $actualArchiveRoot = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($installed.Archive))
    Assert-Test ([string]::Equals($actualArchiveRoot, $expectedArchiveRoot, [StringComparison]::OrdinalIgnoreCase)) `
        "Default archive parent mismatch: expected '$expectedArchiveRoot', got '$actualArchiveRoot'"
    Assert-Test (Test-Path -LiteralPath (Join-Path $installed.Archive 'previous/eclipse.exe')) 'Default archive did not preserve old executable'
    # Set-Location does not necessarily update Environment.CurrentDirectory.
    # Source, destination and explicit archive paths must follow the shell.
    $savedLocation = Get-Location
    $savedProcessDirectory = [Environment]::CurrentDirectory
    $otherWorkingDirectory = Join-Path $root 'process-working-directory'
    New-Item -ItemType Directory -Path $otherWorkingDirectory | Out-Null
    try {
        Set-Location -LiteralPath $root
        # Set-Location may expand an NTFS short name such as a temporary profile.
        $shellDirectory = (Get-Location).ProviderPath
        [Environment]::CurrentDirectory = $otherWorkingDirectory
        $relative = (& $Installer -SourceDir 'old' -InstallDir 'relative-installed' -ArchiveRoot 'relative-archive' -NoShortcut) | ConvertFrom-Json
        Assert-Test ([string]::Equals($relative.InstallDir, (Join-Path $shellDirectory 'relative-installed'), [StringComparison]::OrdinalIgnoreCase)) "Relative installation mismatch: expected '$(Join-Path $shellDirectory 'relative-installed')', got '$($relative.InstallDir)', shell '$((Get-Location).Path)', process '$([Environment]::CurrentDirectory)'"
        Assert-Test (Test-Path -LiteralPath (Join-Path $root 'relative-installed/eclipse.exe')) 'Relative payload was not installed in the shell location'
        Assert-Test ([string]::Equals([IO.Path]::GetDirectoryName($relative.Archive), (Join-Path $shellDirectory 'relative-archive'), [StringComparison]::OrdinalIgnoreCase)) "Relative archive mismatch: expected '$(Join-Path $shellDirectory 'relative-archive')', got '$([IO.Path]::GetDirectoryName($relative.Archive))'"
        Assert-Test (!(Test-Path -LiteralPath (Join-Path $otherWorkingDirectory 'relative-installed'))) 'Installation leaked into the process working directory'
    } finally {
        Set-Location -LiteralPath $savedLocation.Path
        [Environment]::CurrentDirectory = $savedProcessDirectory
    }
    $failed = $false
    try { & $Installer -SourceDir $old -InstallDir ([IO.Path]::GetPathRoot($root)) -NoShortcut | Out-Null }
    catch { $failed = $true; Assert-Test ($_.Exception.Message -like '*drive root*') 'Drive root rejection was not explicit' }
    Assert-Test $failed 'Drive root installation should be rejected before mutation'
    # Validation failures must occur before touching any installed payload.
    $before = Get-TestHashes
    [IO.File]::WriteAllText((Join-Path $old 'eclipse-cli.exe'), 'corrupt payload', $utf8)
    $failed = $false
    try { & $Installer -SourceDir $old -InstallDir $target -ArchiveRoot $archive -NoShortcut | Out-Null }
    catch { $failed = $true }
    Assert-Test $failed 'Corrupt payload was accepted'
    Assert-TestHashes $before
    $legacy = New-TestPackage 'legacy' 1
    & $Installer -SourceDir $legacy -InstallDir $target -ArchiveRoot $archive -NoShortcut | Out-Null
    Assert-Test ((Get-FileHash -LiteralPath (Join-Path $target 'uu.exe')).Hash -eq (Get-FileHash -LiteralPath (Join-Path $legacy 'uu.exe')).Hash) 'Legacy bundle did not install'
    # Renaming the product archives old binaries and preserves user files.
    $previousSdk = New-TestPackage 'previous-sdk' 4
    $renamedTarget = Join-Path $root 'renamed-installed'
    & $Installer -SourceDir $previousSdk -InstallDir $renamedTarget -NoShortcut | Out-Null
    [IO.File]::WriteAllText((Join-Path $renamedTarget 'user-config.json'), 'keep configuration', $utf8)
    $renamed = (& $Installer -SourceDir $new -InstallDir $renamedTarget -NoShortcut) | ConvertFrom-Json
    Assert-Test (Test-Path -LiteralPath (Join-Path $renamedTarget 'eclipse.exe')) 'Renamed GUI missing'
    Assert-Test (!(Test-Path -LiteralPath (Join-Path $renamedTarget 'uu-gui.exe'))) 'Legacy GUI left in active installation'
    Assert-Test (!(Test-Path -LiteralPath (Join-Path $renamedTarget 'uu.exe'))) 'Legacy CLI left in active installation'
    Assert-Test (Test-Path -LiteralPath (Join-Path $renamed.Archive 'previous/uu-gui.exe')) 'Legacy GUI was not archived'
    Assert-Test ([IO.File]::ReadAllText((Join-Path $renamedTarget 'user-config.json')) -eq 'keep configuration') 'Renaming changed user configuration'
    $before = Get-TestHashes
    $unknown = New-TestPackage 'unsupported' 6
    $failed = $false
    try { & $Installer -SourceDir $unknown -InstallDir $target -ArchiveRoot $archive -NoShortcut | Out-Null }
    catch { $failed = $true }
    Assert-Test $failed 'Unknown package format was accepted'
    Assert-TestHashes $before
    # Load only function declarations, then exercise real COM shortcuts entirely
    # inside this test directory. Never scan the user's desktop or Start menu.
    $tokens = $null; $parseErrors = $null
    $ast = [Management.Automation.Language.Parser]::ParseFile($Installer, [ref]$tokens, [ref]$parseErrors)
    Assert-Test ($parseErrors.Count -eq 0) 'Installer has PowerShell parser errors'
    $ast.FindAll({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] }, $false) |
        ForEach-Object { Invoke-Expression $_.Extent.Text }
    $links = Join-Path $root 'shortcuts'
    New-Item -ItemType Directory -Path $links | Out-Null
    $shell = New-Object -ComObject WScript.Shell
    $oldExe = Join-Path $root 'previous/uu-gui.exe'
    $newExe = Join-Path $renamedTarget 'eclipse.exe'
    $ownedTargets = @($oldExe, $newExe)
    $unrelated = Join-Path $links 'Eclipse.lnk'
    $link = $shell.CreateShortcut($unrelated)
    $link.TargetPath = Join-Path $root 'another-program/eclipse.exe'; $link.Save()
    $unrelatedHash = (Get-FileHash -LiteralPath $unrelated).Hash
    $legacyLink = Join-Path $links 'uu-client.lnk'
    $link = $shell.CreateShortcut($legacyLink); $link.TargetPath = $oldExe; $link.Save()
    Update-EclipseShortcut $shell $unrelated $ownedTargets $renamedTarget 'eclipse.exe'
    Update-EclipseShortcut $shell $legacyLink $ownedTargets $renamedTarget 'eclipse.exe'
    Assert-Test ((Get-FileHash -LiteralPath $unrelated).Hash -eq $unrelatedHash) 'Unrelated shortcut changed'
    Assert-Test (!(Test-Path -LiteralPath $legacyLink)) 'Legacy shortcut name remains'
    $migrated = Join-Path $links 'Eclipse (1).lnk'
    Assert-Test ($shell.CreateShortcut($migrated).TargetPath -eq $newExe) 'Owned shortcut was not migrated'
    Assert-Test ((Resolve-EclipseShortcut $shell $links $ownedTargets) -eq $migrated) 'Repeated installation duplicated its shortcut'
    Assert-Test ($shell.CreateShortcut($migrated).WorkingDirectory -eq $renamedTarget) 'Shortcut working directory mismatch'
    [pscustomobject]@{passed=$true;rollback=$true;legacy=$true;shortcuts=$true;user_configuration_preserved=$true;user_processes_stopped=$false} | ConvertTo-Json -Compress
} finally {
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}
