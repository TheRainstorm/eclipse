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
function ProgramRules($program) {
    return @(Get-NetFirewallApplicationFilter -PolicyStore ActiveStore -Program $program -ErrorAction SilentlyContinue | Get-NetFirewallRule | Where-Object { MatchesProgram $_ $program })
}
function RuleEvidence($rules) {
    # Keep each port filter paired with its own address and other restrictions.
    # Merging independent filters could falsely combine IPv4-only TCP with IPv6-only UDP.
    foreach ($r in $rules) {
        $a = $r | Get-NetFirewallAddressFilter; $i = $r | Get-NetFirewallInterfaceFilter
        $t = $r | Get-NetFirewallInterfaceTypeFilter; $svc = $r | Get-NetFirewallServiceFilter
        $sec = $r | Get-NetFirewallSecurityFilter; $port = $r | Get-NetFirewallPortFilter
        $unrestricted = ($i.InterfaceAlias -join ',') -eq 'Any' -and "$($t.InterfaceType)" -eq 'Any' -and "$($svc.Service)" -eq 'Any' -and "$($sec.Authentication)" -eq 'NotRequired' -and "$($sec.LocalUser)" -eq 'Any' -and "$($sec.RemoteUser)" -eq 'Any' -and "$($sec.RemoteMachine)" -eq 'Any' -and ($port.RemotePort -join ',') -eq 'Any'
        @{rule=$r; port=$port; address=$a; unrestricted=$unrestricted}
    }
}
function CoversFamily($addresses, $family) {
    foreach ($address in $addresses) {
        if ($address -eq 'Any') { return $true }
        # Arbitrary subnets and dynamic keywords have an intentionally limited
        # scope. Only a whole-family prefix proves complete family coverage.
        $parts = ([string]$address).Split('/')
        $ip = $null
        if ($parts.Count -eq 2 -and $parts[1] -eq '0' -and [Net.IPAddress]::TryParse($parts[0],[ref]$ip)) {
            if (($family -eq 4 -and $ip.AddressFamily -eq [Net.Sockets.AddressFamily]::InterNetwork) -or ($family -eq 6 -and $ip.AddressFamily -eq [Net.Sockets.AddressFamily]::InterNetworkV6)) { return $true }
        }
    }
    return $false
}
function CoversProtocol($evidence, $profile, $family, $protocol) {
    $candidate = @($evidence | Where-Object {
        $r = $_.rule
        "$($r.Direction)" -eq 'Inbound' -and "$($r.Enabled)" -eq 'True' -and "$($r.Action)" -eq 'Allow' -and ("$($r.Profile)" -eq 'Any' -or "$($r.Profile)" -match $profile) -and $_.unrestricted -and (CoversFamily $_.address.LocalAddress $family) -and (CoversFamily $_.address.RemoteAddress $family)
    })
    $needed = if ($protocol -eq 'TCP') { @($inputData.servicePort) + @(49000..49009) } else { @($inputData.discoveryPort) + @(49000..49009) }
    $protocolValues = if ($protocol -eq 'TCP') { @('TCP','6','Any','256') } else { @('UDP','17','Any','256') }
    foreach ($port in $needed) {
        $covered = $false
        foreach ($e in $candidate) {
            if ("$($e.port.Protocol)" -in $protocolValues -and (Covers $e.port.LocalPort $port)) { $covered = $true; break }
        }
        if (!$covered) { return $false }
    }
    return $true
}
function Report($program) {
    $rules = @(ProgramRules $program)
    $evidence = @(RuleEvidence $rules)
    $profiles = @(Get-NetFirewallProfile -PolicyStore ActiveStore | Select-Object Name,Enabled,DefaultInboundAction,AllowInboundRules,AllowLocalFirewallRules)
    $networks = @(Get-NetConnectionProfile | Select-Object InterfaceAlias,NetworkCategory)
    $warnings = [Collections.Generic.List[string]]::new()
    $coverage = [Collections.Generic.List[object]]::new()
    $activeNames = @($networks | ForEach-Object { if ($_.NetworkCategory -eq 'DomainAuthenticated') {'Domain'} else {[string]$_.NetworkCategory} } | Select-Object -Unique)
    if (!$activeNames.Count) { $warnings.Add('No active network profile; remote reachability is unknown') }
    foreach ($p in $profiles) {
        $v4 = (CoversProtocol $evidence $p.Name 4 'TCP') -and (CoversProtocol $evidence $p.Name 4 'UDP')
        $v6 = (CoversProtocol $evidence $p.Name 6 'TCP') -and (CoversProtocol $evidence $p.Name 6 'UDP')
        $coverage.Add(@{profile=[string]$p.Name;ipv4=$v4;ipv6=$v6})
        if ($p.Name -notin $activeNames -or "$($p.Enabled)" -eq 'False') { continue }
        if ("$($p.AllowInboundRules)" -eq 'False' -or "$($p.AllowLocalFirewallRules)" -eq 'False') { $warnings.Add("$($p.Name): policy prevents local inbound rules") }
        if (!$v4) { $warnings.Add("$($p.Name) IPv4: ports are missing or restricted") }
        if (!$v6) { $warnings.Add("$($p.Name) IPv6: ports are missing or restricted") }
    }
    # Enumerate before filtering: some providers treat an empty server-side
    # Action/Enabled query as an error. Explicit blocks always remain in force.
    $blocks = @(Get-NetFirewallRule -PolicyStore ActiveStore -ErrorAction SilentlyContinue | Where-Object { "$($_.Direction)" -eq 'Inbound' -and "$($_.Enabled)" -eq 'True' -and "$($_.Action)" -eq 'Block' } | Where-Object {
        $app = $_ | Get-NetFirewallApplicationFilter
        $app.Program -eq 'Any' -or [string]::Equals($app.Program,$program,[StringComparison]::OrdinalIgnoreCase)
    })
    if ($blocks.Count) { $warnings.Add('Explicit inbound Block rules may apply; they were preserved') }
    $restricted = @($evidence | Where-Object { !$_.unrestricted -or ($_.address.RemoteAddress -join ',') -ne 'Any' -or ($_.address.LocalAddress -join ',') -ne 'Any' })
    if ($restricted.Count) { $warnings.Add('Existing address/interface/service/security restrictions were preserved; verify the remote connection') }
    return @{program=$program; rules=@($rules | Select-Object Name,Enabled,Action,Profile,Direction); profiles=$profiles; networks=$networks; coverage=@($coverage); warnings=@($warnings); changed=$changes.Count; reachability='unknown'}
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
    if ($inputData.action -in @('ensure','migrate','repair')) {
        $existing = @(ProgramRules $inputData.program)
        $evidence = if ($inputData.action -eq 'repair') { @(RuleEvidence $existing) } else { @() }
        foreach ($protocol in @('TCP','UDP')) {
            # Automatic startup/migration preserve every existing authorization.
            # Explicit repair may supplement missing dual-stack coverage, while
            # leaving all existing Allow/Block rules and their filters intact.
            $needed = !$existing.Count
            if ($inputData.action -eq 'repair') {
                foreach ($profile in @('Domain','Private')) {
                    foreach ($family in @(4,6)) {
                        if (!(CoversProtocol $evidence $profile $family $protocol)) { $needed = $true }
                    }
                }
            }
            if ($needed) {
                $ports = if ($protocol -eq 'TCP') {@([string]$inputData.servicePort,'49000-49009')} else {@([string]$inputData.discoveryPort,'49000-49009')}
                $name = 'Eclipse.Stream.' + [guid]::NewGuid().ToString('N')
                $r = New-NetFirewallRule -PolicyStore PersistentStore -Name $name -DisplayName "Eclipse streaming $protocol" -Group 'Eclipse streaming' -Direction Inbound -Action Allow -Enabled True -Profile Domain,Private -Program $inputData.program -Protocol $protocol -LocalPort $ports -LocalAddress Any -RemoteAddress Any
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
