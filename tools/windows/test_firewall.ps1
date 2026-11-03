# Run elevated in an isolated fixture. Never changes rules for a running program.
param([string]$EngineScript = (Join-Path $PSScriptRoot '../../crates/uu-core/src/firewall/windows.ps1'))
$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$root=Join-Path $env:TEMP ('eclipse-firewall-tests-'+[guid]::NewGuid().ToString('N'))
$engine=Join-Path $root 'engine.ps1'
$old=Join-Path $root 'uu.exe'; $new=Join-Path $root 'eclipse-cli.exe'; $other=Join-Path $root 'other.exe'
$prefix='Eclipse.Firewall.Fixture.'+[guid]::NewGuid().ToString('N')
New-Item -ItemType Directory -Path $root | Out-Null
Copy-Item $EngineScript $engine
foreach($p in @($old,$new,$other)){Copy-Item "$env:SystemRoot/System32/cmd.exe" $p}
function Assert($condition,$message){if(!$condition){throw $message}}
function Invoke-Engine($action,$program=$old,$previous=$null,$changes=$null,$script=$engine,$expectFailure=$false) {
    $start=[Diagnostics.ProcessStartInfo]::new()
    $start.FileName="$env:SystemRoot/System32/WindowsPowerShell/v1.0/powershell.exe"
    $start.Arguments='-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "'+$script+'"'
    $start.UseShellExecute=$false; $start.CreateNoWindow=$true
    $start.RedirectStandardInput=$true; $start.RedirectStandardOutput=$true; $start.RedirectStandardError=$true
    $start.StandardOutputEncoding=[Text.UTF8Encoding]::new($false)
    $process=[Diagnostics.Process]::Start($start)
    $output=$process.StandardOutput.ReadToEndAsync(); $errors=$process.StandardError.ReadToEndAsync()
    $process.StandardInput.WriteLine((@{action=$action;program=$program;oldProgram=$previous;servicePort=48998;discoveryPort=48999;changes=$changes}|ConvertTo-Json -Depth 24 -Compress))
    $process.StandardInput.Close()
    Assert ($process.WaitForExit(90000)) 'Firewall fixture timeout'
    $value=$output.Result|ConvertFrom-Json
    if($expectFailure){Assert (!$value.ok -and $process.ExitCode -ne 0) ("Expected failure was accepted: $action $program $script")}
    else{Assert ($value.ok -and $process.ExitCode -eq 0) ($value.error+' '+$errors.Result)}
    $process.Dispose(); return $value
}
function Program-Rules($program){@(Get-NetFirewallApplicationFilter -PolicyStore PersistentStore -Program $program -ErrorAction SilentlyContinue|Get-NetFirewallRule)}
$passed=[Collections.Generic.List[string]]::new()
try {
    $r=Invoke-Engine inspect; Assert ($r.report.rules.Count -eq 0) 'Inspection created rules'
    $r=Invoke-Engine ensure; Assert ($r.report.changed -eq 2) 'Fresh installation must create TCP and UDP rules';$created=$r.changes
    foreach($rule in (Program-Rules $old)){
        Assert ([int]$rule.Profile -eq 3) 'Fresh rule opened Public'
        Assert ([string]$rule.Action -eq 'Allow') 'Fresh rule is not Allow'
        $ports=$rule|Get-NetFirewallPortFilter
        Assert ('49000-49009' -in $ports.LocalPort) 'Streaming range differs'
    }
    $r=Invoke-Engine ensure; Assert ($r.report.changed -eq 0) 'Repeated startup duplicated rules'
    $r=Invoke-Engine migrate $new $old;Assert ($r.report.changed -eq 2) 'Upgrade did not migrate both rules';$moved=$r.changes
    $null=Invoke-Engine rollback $new $null $moved
    $null=Invoke-Engine rollback $old $null $created
    Assert ((Program-Rules $old).Count -eq 0) 'Rollback left owned rules'
    $passed.Add('fresh, idempotent, upgrade, exact rollback')

    # Preserve restrictions, disabled rules and explicit Block across old/new filenames.
    $null=New-NetFirewallRule -Name ($prefix+'.tcp') -DisplayName $prefix -Direction Inbound -Action Allow -Enabled True -Profile Private -Program $old -Protocol TCP -LocalPort 48998,49000-49003 -RemotePort 1024-65535 -RemoteAddress 192.0.2.0/24 -InterfaceType Wired
    $null=New-NetFirewallRule -Name ($prefix+'.udp') -DisplayName $prefix -Direction Inbound -Action Block -Enabled False -Profile Domain -Program $old -Protocol UDP -LocalPort 49000-49009 -Service Any
    $r=Invoke-Engine migrate $new $old; Assert ($r.report.changed -eq 2) 'Scoped rules not migrated';$moved=$r.changes
    $disabled=Get-NetFirewallRule -PolicyStore PersistentStore -Name ($prefix+'.udp')
    Assert ([string]$disabled.Enabled -eq 'False' -and [string]$disabled.Action -eq 'Block') 'Disabled/Block rule broadened'
    $r=Invoke-Engine ensure $new;Assert ($r.report.changed -eq 0) 'Restricted authorization was broadened'
    $null=Invoke-Engine rollback $new $null $moved
    $passed.Add('restricted profile/address/port/interface, disabled Block, old filenames')

    # Never merge independent authorizations for both program paths.
    $null=New-NetFirewallRule -Name ($prefix+'.other') -DisplayName $prefix -Direction Inbound -Action Allow -Profile Private -Program $new -Protocol UDP -LocalPort 49009
    $r=Invoke-Engine migrate $new $old $null $engine $true
    Assert ((Program-Rules $old).Count -eq 2) 'Conflict modified old rules'
    Get-NetFirewallRule -PolicyStore PersistentStore -Name ($prefix+'.other') | Remove-NetFirewallRule
    $passed.Add('new-path conflict leaves both sides untouched')

    # A concurrent administrator edit must prevent compensation from overwriting it.
    $r=Invoke-Engine migrate $new $old;$moved=$r.changes
    $changedName=$moved[0].name
    Set-NetFirewallRule -PolicyStore PersistentStore -Name $changedName -Description 'concurrent administrator change'
    $r=Invoke-Engine rollback $new $null $moved $engine $true
    Assert ((Get-NetFirewallRule -PolicyStore PersistentStore -Name $changedName).Description -eq 'concurrent administrator change') 'Rollback overwrote concurrent edit'
    $passed.Add('postimage conflict preserves concurrent administrator changes')

    # Inject an ordinary second-write error while using the real first rule/API.
    $fault=Join-Path $root 'fault.ps1'
    $preamble=@'
Import-Module NetSecurity
$script:writes=0
function New-NetFirewallRule {
 [CmdletBinding()]param($PolicyStore,$Name,$DisplayName,$Group,$Direction,$Action,$Enabled,$Profile,$Program,$Protocol,$LocalPort)
 $script:writes++;if($script:writes -eq 2){throw 'injected second rule failure'}
 NetSecurity\New-NetFirewallRule @PSBoundParameters
}
'@
    [IO.File]::WriteAllText($fault,[IO.File]::ReadAllText($engine).Replace('Import-Module NetSecurity -ErrorAction Stop',$preamble),[Text.UTF8Encoding]::new($true))
    $r=Invoke-Engine ensure $other $null $null $fault $true
    Assert ((Program-Rules $other).Count -eq 0) 'Partial create was not compensated'
    $passed.Add('second rule failure compensates only the first owned write')
    @{ok=$true;passed=@($passed)}|ConvertTo-Json -Compress
} finally {
    # All paths and rule names were created by this fixture; no global imports.
    foreach($program in @($old,$new,$other)){Program-Rules $program | Remove-NetFirewallRule}
    Remove-Item -LiteralPath $root -Recurse -Force
}
