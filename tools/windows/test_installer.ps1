# Exercise only generated package files in a newly owned temporary directory.
param([string]$Installer = (Join-Path $PSScriptRoot 'install.ps1'))
$ErrorActionPreference = 'Stop'
$root = Join-Path $env:TEMP ('uu-installer-test-' + [Guid]::NewGuid().ToString('N'))
$baseFiles = @('uu-gui.exe','uu.exe','libgcc_s_seh-1.dll','libstdc++-6.dll','README.txt','install.ps1',
               'LICENSE','LICENSE-MIT','LICENSE-APACHE','THIRD-PARTY-NOTICES.txt')
$target = Join-Path $root 'installed'
$archive = Join-Path $root 'archive'
$utf8 = New-Object System.Text.UTF8Encoding($false)
function New-TestPackage([string]$Name, [int]$Format = 2) {
    $dir = Join-Path $root $Name
    New-Item -ItemType Directory -Path $dir | Out-Null
    $files = @($baseFiles)
    if ($Format -eq 2) { $files += 'libvpl.dll' }
    $hashes = @{}
    foreach ($file in $files) {
        if ($file -eq 'install.ps1') { Copy-Item -LiteralPath $Installer -Destination (Join-Path $dir $file) }
        else { [IO.File]::WriteAllText((Join-Path $dir $file), "$Name/$file", $utf8) }
        $hashes[$file] = (Get-FileHash -LiteralPath (Join-Path $dir $file)).Hash.ToLowerInvariant()
    }
    $manifest = @{source_commit=('a' * 40);sha256=$hashes}
    if ($Format -ne 1) { $manifest.format_version = $Format }
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
    $locked = [IO.File]::Open((Join-Path $target 'libvpl.dll'), [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        $failed = $false
        try { & $Installer -SourceDir $new -InstallDir $target -ArchiveRoot $archive -NoShortcut | Out-Null }
        catch { $failed = $true; Assert-Test ($_.Exception.Message -like '*package files were restored*') 'Installer did not report rollback' }
        Assert-Test $failed 'A sharing violation should fail installation'
        Assert-TestHashes $before
    } finally { $locked.Dispose() }
    & $Installer -SourceDir $new -InstallDir $target -ArchiveRoot $archive -NoShortcut | Out-Null
    foreach ($file in @($baseFiles) + @('libvpl.dll', 'version.json')) {
        Assert-Test ((Get-FileHash -LiteralPath (Join-Path $target $file)).Hash -eq (Get-FileHash -LiteralPath (Join-Path $new $file)).Hash) "New payload mismatch: $file"
    }
    Assert-Test ([IO.File]::ReadAllText((Join-Path $target 'user-config.json')) -eq 'preserved user settings') 'User settings changed'
    $defaultTarget = Join-Path $root 'default-archive-installed'
    & $Installer -SourceDir $old -InstallDir $defaultTarget -NoShortcut | Out-Null
    $installed = (& $Installer -SourceDir $new -InstallDir $defaultTarget -NoShortcut) | ConvertFrom-Json
    Assert-Test ($installed.Archive.StartsWith((Join-Path $defaultTarget '.upgrade-archive'))) 'Default archive must stay inside installation on its backing volume'
    Assert-Test (Test-Path -LiteralPath (Join-Path $installed.Archive 'previous/uu-gui.exe')) 'Default archive did not preserve old executable'
    $failed = $false
    try { & $Installer -SourceDir $old -InstallDir ([IO.Path]::GetPathRoot($root)) -NoShortcut | Out-Null }
    catch { $failed = $true; Assert-Test ($_.Exception.Message -like '*drive root*') 'Drive root rejection was not explicit' }
    Assert-Test $failed 'Drive root installation should be rejected before mutation'
    # Validation failures must occur before touching any installed payload.
    $before = Get-TestHashes
    [IO.File]::WriteAllText((Join-Path $old 'uu.exe'), 'corrupt payload', $utf8)
    $failed = $false
    try { & $Installer -SourceDir $old -InstallDir $target -ArchiveRoot $archive -NoShortcut | Out-Null }
    catch { $failed = $true }
    Assert-Test $failed 'Corrupt payload was accepted'
    Assert-TestHashes $before
    $legacy = New-TestPackage 'legacy' 1
    & $Installer -SourceDir $legacy -InstallDir $target -ArchiveRoot $archive -NoShortcut | Out-Null
    Assert-Test ((Get-FileHash -LiteralPath (Join-Path $target 'uu.exe')).Hash -eq (Get-FileHash -LiteralPath (Join-Path $legacy 'uu.exe')).Hash) 'Legacy bundle did not install'
    $before = Get-TestHashes
    $unknown = New-TestPackage 'unsupported' 3
    $failed = $false
    try { & $Installer -SourceDir $unknown -InstallDir $target -ArchiveRoot $archive -NoShortcut | Out-Null }
    catch { $failed = $true }
    Assert-Test $failed 'Unknown package format was accepted'
    Assert-TestHashes $before
    [pscustomobject]@{passed=$true;rollback=$true;legacy=$true;user_configuration_preserved=$true;user_processes_stopped=$false} | ConvertTo-Json -Compress
} finally {
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}
