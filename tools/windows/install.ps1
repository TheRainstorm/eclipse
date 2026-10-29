# Keep loaded images on their original volume when archiving an upgrade.
param(
    [string]$SourceDir = $PSScriptRoot,
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\Eclipse'),
    [string]$ArchiveRoot = '',
    [switch]$NoShortcut
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
function Resolve-PackagePath([string]$Value) {
    # PowerShell locations and the process working directory can differ.
    $provider = $null
    $drive = $null
    $path = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Value, [ref]$provider, [ref]$drive)
    if ($provider.Name -ne 'FileSystem') { throw 'Package paths must use the FileSystem provider' }
    $full = [IO.Path]::GetFullPath($path)
    if ($full.Length -gt [IO.Path]::GetPathRoot($full).Length) {
        return $full.TrimEnd([IO.Path]::DirectorySeparatorChar)
    }
    return $full
}
function Resolve-EclipseShortcut($Shell, [string]$Directory, [string[]]$OwnedTargets) {
    for ($index = 0; ; $index++) {
        $name = if ($index -eq 0) { 'Eclipse.lnk' } else { "Eclipse ($index).lnk" }
        $candidate = Join-Path $Directory $name
        if (!(Test-Path -LiteralPath $candidate)) { return $candidate }
        if ((Test-Path -LiteralPath $candidate -PathType Leaf) -and
            $Shell.CreateShortcut($candidate).TargetPath -in $OwnedTargets) { return $candidate }
    }
}
function Set-EclipseShortcut($Shell, [string]$Path, [string]$Directory, [string]$GuiName) {
    $link = $Shell.CreateShortcut($Path)
    $link.TargetPath = Join-Path $Directory $GuiName
    $link.WorkingDirectory = $Directory
    $link.IconLocation = (Join-Path $Directory $GuiName) + ',0'
    $link.Description = 'Eclipse - remote streaming'
    $link.Save()
}
function Update-EclipseShortcut($Shell, [string]$Path, [string[]]$OwnedTargets, [string]$Directory, [string]$GuiName) {
    # Shell links can use non-file targets. Compare only known application paths.
    if ($Shell.CreateShortcut($Path).TargetPath -notin $OwnedTargets) { return }
    Set-EclipseShortcut $Shell $Path $Directory $GuiName
    if ([IO.Path]::GetFileNameWithoutExtension($Path) -in @('UU Remote Rust','uu-client')) {
        $destination = Resolve-EclipseShortcut $Shell ([IO.Path]::GetDirectoryName($Path)) $OwnedTargets
        Move-Item -LiteralPath $Path -Destination $destination -Force
    }
}
$SourceDir = Resolve-PackagePath $SourceDir
$InstallDir = Resolve-PackagePath $InstallDir
if ($InstallDir -eq [IO.Path]::GetPathRoot($InstallDir)) { throw 'InstallDir must not be a drive root' }
# A junction may point at another volume. Keeping the default archive below
# InstallDir guarantees a rename rather than a cross-volume copy/delete.
if ([string]::IsNullOrWhiteSpace($ArchiveRoot)) { $ArchiveRoot = Join-Path $InstallDir '.upgrade-archive' }
$ArchiveRoot = Resolve-PackagePath $ArchiveRoot
$manifest = Get-Content -LiteralPath (Join-Path $SourceDir 'version.json') -Raw | ConvertFrom-Json
$format = 1
if ($null -ne $manifest.format_version) {
    if ($manifest.format_version -isnot [int] -and $manifest.format_version -isnot [long]) {
        throw 'Invalid package format version'
    }
    $format = $manifest.format_version
}
if ($format -notin @(1, 2, 3, 4, 5)) { throw 'Unsupported package format version' }
if ($manifest.source_commit -cnotmatch '^[0-9a-f]{40}$') { throw 'Invalid source commit' }
$required = @('uu-gui.exe','uu.exe','libgcc_s_seh-1.dll','libstdc++-6.dll','README.txt','install.ps1',
              'LICENSE','LICENSE-MIT','LICENSE-APACHE','THIRD-PARTY-NOTICES.txt')
if ($format -eq 5) { $required[0] = 'eclipse.exe'; $required[1] = 'eclipse-cli.exe' }
$guiName = $required[0]
if ($format -ge 2) { $required += 'libvpl.dll' }
if ($format -ge 3) { $required += 'libpyrowave-shared-1.dll' }
if ($format -ge 4) {
    $runtime = @($manifest.plugin_runtime)
    if ($runtime.Count -ne 2 -or $runtime[0] -cne 'uu_plugin_sdk.dll' -or
        $runtime[1] -isnot [string] -or $runtime[1] -cnotmatch '^std-[0-9a-f]+\.dll$' -or
        $manifest.plugin_sdk_id -cnotmatch '^[0-9a-f]{64}$') { throw 'Invalid plugin SDK runtime manifest' }
    $required += $runtime
}
$hashNames = @($manifest.sha256.PSObject.Properties.Name)
if ($hashNames.Count -ne $required.Count -or @($hashNames | Where-Object { $_ -cnotin $required }).Count) {
    throw 'Invalid package hash manifest'
}
foreach ($name in $required) {
    $file = Join-Path $SourceDir $name
    if (!(Test-Path -LiteralPath $file -PathType Leaf)) { throw "Package missing: $name" }
    $expected = $manifest.sha256.$name
    if ($expected -cnotmatch '^[0-9a-f]{64}$' -or (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ne $expected) {
        throw "Package hash mismatch: $name"
    }
}
$stamp = (Get-Date -Format 'yyyyMMdd-HHmmss-fff') + '-' + [Guid]::NewGuid().ToString('N').Substring(0,8)
$archive = Join-Path $ArchiveRoot $stamp
New-Item -ItemType Directory -Force $InstallDir | Out-Null
# Validate and back up all package files before mutation. User configuration is outside this allowlist.
if (!$SourceDir.Equals($InstallDir, [StringComparison]::OrdinalIgnoreCase)) {
    $previous = Join-Path $archive 'previous'
    New-Item -ItemType Directory -Force $previous | Out-Null
    $payload = @($required) + @('version.json')
    $retired = @()
    if ($format -eq 5) { $retired = @('uu-gui.exe','uu.exe') }
    $oldHashes = @{}
    foreach ($name in @($payload) + $retired) {
        $target = Join-Path $InstallDir $name
        if (Test-Path -LiteralPath $target) {
            if (!(Test-Path -LiteralPath $target -PathType Leaf)) { throw "Installed path is not a file: $name" }
            Copy-Item -LiteralPath $target -Destination (Join-Path $previous $name) -Force
            $oldHashes[$name] = (Get-FileHash -LiteralPath $target).Hash
        }
    }
    $changed = New-Object 'System.Collections.Generic.List[string]'
    try {
        foreach ($name in $retired) {
            $target = Join-Path $InstallDir $name
            if (Test-Path -LiteralPath $target) {
                Move-Item -LiteralPath $target -Destination (Join-Path $previous "running-$name") -Force
                $changed.Add($name)
            }
        }
        foreach ($name in $payload) {
            $target = Join-Path $InstallDir $name
            $source = Join-Path $SourceDir $name
            $digest = (Get-FileHash -LiteralPath $source).Hash
            if ((Test-Path -LiteralPath $target) -and (Get-FileHash -LiteralPath $target).Hash -eq $digest) { continue }
            if ([IO.Path]::GetExtension($name) -in @('.exe', '.dll') -and (Test-Path -LiteralPath $target)) {
                # Loaded images can be renamed while their processes keep running. A sharing violation
                # leaves this file untouched and rolls back the earlier payload changes.
                Move-Item -LiteralPath $target -Destination (Join-Path $previous "running-$name") -Force
            }
            $changed.Add($name)
            Copy-Item -LiteralPath $source -Destination $target -Force
            if ((Get-FileHash -LiteralPath $target).Hash -ne $digest) { throw "Installed hash mismatch: $name" }
        }
    } catch {
        $failure = $_.Exception.Message
        $rollbackErrors = New-Object 'System.Collections.Generic.List[string]'
        for ($index = $changed.Count - 1; $index -ge 0; $index--) {
            $name = $changed[$index]
            $target = Join-Path $InstallDir $name
            try {
                if ($oldHashes.ContainsKey($name) -and (Test-Path -LiteralPath $target) -and
                    (Get-FileHash -LiteralPath $target).Hash -eq $oldHashes[$name]) { continue }
                if (Test-Path -LiteralPath $target) {
                    $failed = Join-Path $archive 'failed'
                    New-Item -ItemType Directory -Force $failed | Out-Null
                    Move-Item -LiteralPath $target -Destination (Join-Path $failed $name) -Force
                }
                if ($oldHashes.ContainsKey($name)) {
                    Copy-Item -LiteralPath (Join-Path $previous $name) -Destination $target -Force
                    if ((Get-FileHash -LiteralPath $target).Hash -ne $oldHashes[$name]) { throw 'Restored hash mismatch' }
                }
            } catch { $rollbackErrors.Add($name + ': ' + $_.Exception.Message) }
        }
        if ($rollbackErrors.Count) { throw "Installation failed: $failure; rollback incomplete: $($rollbackErrors -join '; '); backup: $previous" }
        throw "Installation failed and package files were restored: $failure; backup: $previous"
    }
}
foreach ($name in @('rtx-auto','hud-recovery')) {
    $legacy = Join-Path $InstallDir $name
    if (Test-Path $legacy) {
        New-Item -ItemType Directory -Force $archive | Out-Null
        Move-Item $legacy (Join-Path $archive $name)
    }
}
Get-ChildItem $InstallDir -File | Where-Object {
    $_.Name -like 'uu-gui.pre-*.exe' -or $_.Name -like 'uu-gui.before-*.exe'
} | ForEach-Object {
    New-Item -ItemType Directory -Force $archive | Out-Null
    Move-Item $_.FullName (Join-Path $archive $_.Name)
}
$shortcut = $null
if (!$NoShortcut) {
    $shell = New-Object -ComObject WScript.Shell
    $ownedTargets = @(
        (Join-Path $InstallDir 'uu-gui.exe'), (Join-Path $InstallDir 'eclipse.exe'),
        (Join-Path $env:LOCALAPPDATA 'Programs\uu-client\uu-gui.exe'),
        (Join-Path $env:LOCALAPPDATA 'Programs\uu-client\eclipse.exe'),
        (Join-Path $env:USERPROFILE 'uu-gui-demo\uu-gui.exe')
    )
    $roots = @([Environment]::GetFolderPath('Desktop'), [Environment]::GetFolderPath('StartMenu'))
    foreach ($root in $roots) {
        Get-ChildItem $root -Filter '*.lnk' -Recurse -ErrorAction SilentlyContinue | ForEach-Object {
            Update-EclipseShortcut $shell $_.FullName $ownedTargets $InstallDir $guiName
        }
    }
    $shortcut = Resolve-EclipseShortcut $shell ([Environment]::GetFolderPath('Desktop')) $ownedTargets
    Set-EclipseShortcut $shell $shortcut $InstallDir $guiName
}
foreach ($name in $required) {
    if ((Get-FileHash -LiteralPath (Join-Path $InstallDir $name)).Hash -ne $manifest.sha256.$name) {
        throw "Installed hash mismatch: $name"
    }
}
[pscustomobject]@{
    InstallDir=$InstallDir; Executable=(Join-Path $InstallDir $guiName)
    SourceCommit=$manifest.source_commit; Shortcut=$shortcut; Archive=$archive
} | ConvertTo-Json -Compress
