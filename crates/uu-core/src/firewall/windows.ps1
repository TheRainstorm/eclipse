# Embedded trusted code; all caller data arrives as JSON on stdin.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$inputData = [Console]::In.ReadToEnd() | ConvertFrom-Json
Import-Module NetSecurity -ErrorAction Stop
$changes = [Collections.Generic.List[object]]::new()
function Properties($object) {
    $map = [ordered]@{}
    foreach ($p in ($object.CimInstanceProperties | Sort-Object Name)) {
        if ($p.Name -notin @('Status','StatusCode','PrimaryStatus','EnforcementStatus','PolicyStoreSource','PolicyStoreSourceType')) { $map[$p.Name] = $p.Value }
    }
    return $map
}
function Snapshot($rule) {
    $s = [ordered]@{rule=(Properties $rule)}
    foreach ($kind in @('Application','Address','Port','Interface','InterfaceType','Service','Security')) {
        $filter = & "Get-NetFirewall${kind}Filter" -AssociatedNetFirewallRule $rule
        $s[$kind] = @(foreach ($f in $filter) { Properties $f })
    }
    return ($s | ConvertTo-Json -Depth 16 -Compress)
}
function FindLocal($name) { Get-NetFirewallRule -PolicyStore PersistentStore -Name $name -ErrorAction SilentlyContinue }
function Undo($items) {
    $failures = @()
    foreach ($c in @($items | Sort-Object sequence -Descending)) {
        try {
            $r = FindLocal $c.name
            if ($r -and $c.before -and (Snapshot $r) -ceq $c.before) { continue }
            if (!$r -or (Snapshot $r) -cne $c.after) { throw "Rule changed concurrently: $($c.name); left untouched" }
            if ($c.before) {
                $r | Get-NetFirewallApplicationFilter | Set-NetFirewallApplicationFilter -Program $c.oldProgram | Out-Null
                if ((Snapshot (FindLocal $c.name)) -cne $c.before) { throw "Rollback readback differs: $($c.name)" }
            } else { $r | Remove-NetFirewallRule }
        } catch { $failures += $_.Exception.Message }
    }
    if ($failures.Count) { throw ($failures -join '; ') }
}
function MatchesProgram($rule, $program) {
    $app = $rule | Get-NetFirewallApplicationFilter
    return [string]::Equals($app.Program, $program, [StringComparison]::OrdinalIgnoreCase)
}
function Covers($values, $port) {
    foreach ($v in $values) {
        if ($v -eq 'Any') { return $true }
        if ($v -match '^([0-9]+)-([0-9]+)$' -and $port -ge [int]$Matches[1] -and $port -le [int]$Matches[2]) { return $true }
        if ($v -eq [string]$port) { return $true }
    }
    return $false
}
function Report($program) {
    $rules = @(Get-NetFirewallApplicationFilter -PolicyStore ActiveStore -Program $program -ErrorAction SilentlyContinue | Get-NetFirewallRule | Where-Object { MatchesProgram $_ $program })
    $profiles = @(Get-NetFirewallProfile -PolicyStore ActiveStore | Select-Object Name,Enabled,DefaultInboundAction,AllowInboundRules,AllowLocalFirewallRules)
    $networks = @(Get-NetConnectionProfile | Select-Object InterfaceAlias,NetworkCategory)
    $warnings = [Collections.Generic.List[string]]::new()
    $activeNames = @($networks | ForEach-Object { if ($_.NetworkCategory -eq 'DomainAuthenticated') {'Domain'} else {[string]$_.NetworkCategory} } | Select-Object -Unique)
    if (!$activeNames.Count) { $warnings.Add('No active network profile; remote reachability is unknown') }
    foreach ($p in $profiles) {
        if ($p.Name -notin $activeNames -or "$($p.Enabled)" -eq 'False') { continue }
        if ("$($p.AllowInboundRules)" -eq 'False' -or "$($p.AllowLocalFirewallRules)" -eq 'False') { $warnings.Add("$($p.Name): policy prevents local inbound rules") }
        $candidate = @($rules | Where-Object { "$($_.Direction)" -eq 'Inbound' -and "$($_.Enabled)" -eq 'True' -and "$($_.Action)" -eq 'Allow' -and ("$($_.Profile)" -eq 'Any' -or "$($_.Profile)" -match $p.Name) })
        $portFilters = @($candidate | Get-NetFirewallPortFilter)
        foreach ($protocol in @('TCP','UDP')) {
            $needed = if ($protocol -eq 'TCP') { @($inputData.servicePort) + @(49000..49009) } else { @($inputData.discoveryPort) + @(49000..49009) }
            foreach ($port in $needed) {
                $covered = $false
                foreach ($f in $portFilters) {
                    if ("$($f.Protocol)" -in @($protocol,'Any') -and (Covers $f.LocalPort $port)) { $covered = $true; break }
                }
                if (!$covered) { $warnings.Add("$($p.Name): $protocol ports are missing or restricted"); break }
            }
        }
    }
    # Explicit Block wins over Allow. Report potential overlaps without changing it.
    # Some NetSecurity providers fail the whole CIM query when server-side
    # Action/Enabled filters have no matching rows. Enumerate first and filter
    # locally so an empty block-rule set is a valid report, not an update error.
    $blocks = @(Get-NetFirewallRule -PolicyStore ActiveStore -ErrorAction SilentlyContinue | Where-Object { "$($_.Direction)" -eq 'Inbound' -and "$($_.Enabled)" -eq 'True' -and "$($_.Action)" -eq 'Block' } | Where-Object {
        $app = $_ | Get-NetFirewallApplicationFilter
        $app.Program -eq 'Any' -or [string]::Equals($app.Program,$program,[StringComparison]::OrdinalIgnoreCase)
    })
    if ($blocks.Count) { $warnings.Add('Explicit inbound Block rules may apply; they were preserved') }
    # A matching allow can still be scoped to addresses/interfaces/services/security.
    $restricted = @($rules | Where-Object {
        $a = $_ | Get-NetFirewallAddressFilter; $i = $_ | Get-NetFirewallInterfaceFilter
        $t = $_ | Get-NetFirewallInterfaceTypeFilter; $svc = $_ | Get-NetFirewallServiceFilter
        $sec = $_ | Get-NetFirewallSecurityFilter; $port = $_ | Get-NetFirewallPortFilter
        ($a.RemoteAddress -join ',') -ne 'Any' -or ($a.LocalAddress -join ',') -ne 'Any' -or ($i.InterfaceAlias -join ',') -ne 'Any' -or "$($t.InterfaceType)" -ne 'Any' -or "$($svc.Service)" -ne 'Any' -or "$($sec.Authentication)" -ne 'NotRequired' -or "$($sec.LocalUser)" -ne 'Any' -or "$($sec.RemoteUser)" -ne 'Any' -or "$($sec.RemoteMachine)" -ne 'Any' -or ($port.RemotePort -join ',') -ne 'Any'
    })
    if ($restricted.Count) { $warnings.Add('Existing address/interface/service/security restrictions were preserved; verify the remote connection') }
    return @{program=$program; rules=@($rules | Select-Object Name,Enabled,Action,Profile,Direction); profiles=$profiles; networks=$networks; warnings=@($warnings); changed=$changes.Count; reachability='unknown'}
}
$mutex = $null; $ownsMutex = $false
try {
    if ($inputData.action -ne 'inspect') {
        $security = [Security.AccessControl.MutexSecurity]::new()
        $security.SetSecurityDescriptorSddlForm('D:P(A;;GA;;;SY)(A;;GA;;;BA)')
        $created = $false
        $mutex = [Threading.Mutex]::new($false,'Global\EclipseRemoteHost.Firewall.v1',[ref]$created,$security)
        try { $ownsMutex = $mutex.WaitOne(30000) } catch [Threading.AbandonedMutexException] { $ownsMutex = $true }
        if (!$ownsMutex) { throw 'Another firewall operation is still running' }
    }
    if ($inputData.action -eq 'rollback') { Undo $inputData.changes; @{ok=$true} | ConvertTo-Json -Compress; exit 0 }
    if ($inputData.action -eq 'migrate' -and $inputData.oldProgram -and $inputData.oldProgram -ine $inputData.program) {
        $old = @(Get-NetFirewallApplicationFilter -PolicyStore PersistentStore -Program $inputData.oldProgram -ErrorAction SilentlyContinue | Get-NetFirewallRule | Where-Object { MatchesProgram $_ $inputData.oldProgram })
        $new = @(Get-NetFirewallApplicationFilter -PolicyStore PersistentStore -Program $inputData.program -ErrorAction SilentlyContinue | Get-NetFirewallRule | Where-Object { MatchesProgram $_ $inputData.program })
        if ($old.Count -and $new.Count) { throw 'Both old and new program paths have local rules; migration requires review' }
        foreach ($r in $old) {
            $before = Snapshot $r
            $expected = $before | ConvertFrom-Json
            foreach ($app in $expected.Application) { $app.AppPath = $inputData.program }
            $after = $expected | ConvertTo-Json -Depth 16 -Compress
            $changes.Add(@{sequence=$changes.Count;name=$r.Name;before=$before;after=$after;oldProgram=$inputData.oldProgram})
            $r | Get-NetFirewallApplicationFilter | Set-NetFirewallApplicationFilter -Program $inputData.program | Out-Null
            if ((Snapshot (FindLocal $r.Name)) -cne $after) { throw 'Migration changed fields besides Program' }
        }
    }
    if ($inputData.action -in @('ensure','migrate')) {
        $existing = @(Get-NetFirewallApplicationFilter -PolicyStore ActiveStore -Program $inputData.program -ErrorAction SilentlyContinue | Get-NetFirewallRule | Where-Object { MatchesProgram $_ $inputData.program })
        # Any existing authorization/denial is deliberate: never broaden or enable it.
        if (!$existing.Count) {
            foreach ($protocol in @('TCP','UDP')) {
                $ports = if ($protocol -eq 'TCP') {@([string]$inputData.servicePort,'49000-49009')} else {@([string]$inputData.discoveryPort,'49000-49009')}
                $name = 'Eclipse.Stream.' + [guid]::NewGuid().ToString('N')
                $r = New-NetFirewallRule -PolicyStore PersistentStore -Name $name -DisplayName "Eclipse streaming $protocol" -Group 'Eclipse streaming' -Direction Inbound -Action Allow -Enabled True -Profile Domain,Private -Program $inputData.program -Protocol $protocol -LocalPort $ports
                $changes.Add(@{sequence=$changes.Count;name=$name;before=$null;after=(Snapshot (FindLocal $name));oldProgram=$null})
            }
        }
    }
    $report = Report $inputData.program
    @{ok=$true;report=$report;changes=@($changes)} | ConvertTo-Json -Depth 20 -Compress
} catch {
    $message = $_.Exception.Message
    try { Undo @($changes) } catch { $message += '; precise rollback failed: ' + $_.Exception.Message }
    @{ok=$false;error=$message} | ConvertTo-Json -Depth 8 -Compress
    exit 1
}
finally {
    if ($ownsMutex) { $mutex.ReleaseMutex() }
    if ($mutex) { $mutex.Dispose() }
}
