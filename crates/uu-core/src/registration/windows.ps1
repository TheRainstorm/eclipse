# Read-only inventory for the UU device registration form. No serial numbers.
$ErrorActionPreference = 'SilentlyContinue'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$os = Get-CimInstance Win32_OperatingSystem
$board = Get-CimInstance Win32_BaseBoard | Select-Object -First 1
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
$gpus = @(Get-CimInstance Win32_VideoController | Select-Object -First 8)
$nic = Get-CimInstance Win32_NetworkAdapter | Where-Object { $_.PhysicalAdapter -and $_.NetEnabled -and $_.MACAddress } | Sort-Object InterfaceIndex | Select-Object -First 1
$display = $gpus | Where-Object { $_.CurrentHorizontalResolution -gt 0 -and $_.CurrentVerticalResolution -gt 0 } | Select-Object -First 1
$screen = if ($display) { '{0}x{1}' -f $display.CurrentHorizontalResolution,$display.CurrentVerticalResolution } else { 'unavailable' }
[pscustomobject]@{
    os = if ($os) { ('{0} ({1}, build {2})' -f $os.Caption,$os.Version,$os.BuildNumber).Trim() } else { 'Windows' }
    base_board = ('{0} {1}' -f $board.Manufacturer,$board.Product).Trim()
    cpu = [string]$cpu.Name
    video = @($gpus | ForEach-Object { [string]$_.Name })
    mac = [string]$nic.MACAddress
    memory = [uint64]($os.TotalVisibleMemorySize / 1024)
    screen = $screen
} | ConvertTo-Json -Compress
