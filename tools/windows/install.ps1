# One Windows installation directory; legacy copies are archived outside it.
param(
    [string]$SourceDir = $PSScriptRoot,
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\uu-client'),
    [string]$ArchiveRoot = (Join-Path $env:USERPROFILE 'uu-gui-archive'),
    [switch]$NoShortcut
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$SourceDir = [IO.Path]::GetFullPath($SourceDir).TrimEnd([IO.Path]::DirectorySeparatorChar)
$InstallDir = [IO.Path]::GetFullPath($InstallDir).TrimEnd([IO.Path]::DirectorySeparatorChar)
$manifest = Get-Content (Join-Path $SourceDir 'version.json') -Raw | ConvertFrom-Json
$required = @('uu-gui.exe','uu.exe','libgcc_s_seh-1.dll','libstdc++-6.dll','README.txt','install.ps1')
foreach ($name in $required) {
    $file = Join-Path $SourceDir $name
    if (!(Test-Path $file -PathType Leaf)) { throw "Package missing: $name" }
    $expected = $manifest.sha256.$name
    if (!$expected -or (Get-FileHash $file -Algorithm SHA256).Hash -ne $expected) {
        throw "Package hash mismatch: $name"
    }
}
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss-fff'
$archive = Join-Path $ArchiveRoot $stamp
New-Item -ItemType Directory -Force $InstallDir | Out-Null
# Finish validation before moving any installed files. Never stop user processes.
if ($SourceDir -ne $InstallDir) {
    New-Item -ItemType Directory -Force (Join-Path $archive 'previous') | Out-Null
    foreach ($name in $required + @('version.json')) {
        $target = Join-Path $InstallDir $name
        if (Test-Path $target) { Copy-Item $target (Join-Path $archive "previous\$name") -Force }
    }
    foreach ($name in $required + @('version.json')) {
        $target = Join-Path $InstallDir $name
        if ((Test-Path $target) -and (Get-FileHash $target).Hash -eq (Get-FileHash (Join-Path $SourceDir $name)).Hash) { continue }
        if ($name -in @('uu-gui.exe','uu.exe') -and (Test-Path $target)) {
            Move-Item $target (Join-Path $archive "previous\running-$name") -Force
        }
        Copy-Item (Join-Path $SourceDir $name) $target -Force
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
    $roots = @([Environment]::GetFolderPath('Desktop'), [Environment]::GetFolderPath('StartMenu'))
    foreach ($root in $roots) {
        Get-ChildItem $root -Filter '*.lnk' -Recurse -ErrorAction SilentlyContinue | ForEach-Object {
            $link = $shell.CreateShortcut($_.FullName)
            $previousExe = Join-Path (Join-Path $env:USERPROFILE 'uu-gui-demo') 'uu-gui.exe'
            if (($link.TargetPath.StartsWith($InstallDir + '\', [StringComparison]::OrdinalIgnoreCase) -and
                [IO.Path]::GetFileName($link.TargetPath) -eq 'uu-gui.exe') -or
                $link.TargetPath.Equals($previousExe, [StringComparison]::OrdinalIgnoreCase)) {
                $link.TargetPath = Join-Path $InstallDir 'uu-gui.exe'
                $link.WorkingDirectory = $InstallDir
                $link.IconLocation = (Join-Path $InstallDir 'uu-gui.exe') + ',0'
                $link.Save()
                if ($_.BaseName -eq 'UU Remote Rust') {
                    Move-Item -LiteralPath $_.FullName -Destination (Join-Path $_.DirectoryName 'uu-client.lnk') -Force
                }
            }
        }
    }
    $shortcut = Join-Path ([Environment]::GetFolderPath('Desktop')) 'uu-client.lnk'
    $link = $shell.CreateShortcut($shortcut)
    $link.TargetPath = Join-Path $InstallDir 'uu-gui.exe'
    $link.WorkingDirectory = $InstallDir
    $link.IconLocation = (Join-Path $InstallDir 'uu-gui.exe') + ',0'
    $link.Description = 'uu-client - canonical installation'
    $link.Save()
}
foreach ($name in $required) {
    if ((Get-FileHash (Join-Path $InstallDir $name)).Hash -ne $manifest.sha256.$name) {
        throw "Installed hash mismatch: $name"
    }
}
[pscustomobject]@{
    InstallDir=$InstallDir; Executable=(Join-Path $InstallDir 'uu-gui.exe')
    SourceCommit=$manifest.source_commit; Shortcut=$shortcut; Archive=$archive
} | ConvertTo-Json -Compress
