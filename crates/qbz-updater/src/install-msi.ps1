param(
    [Parameter(Mandatory=$true)][int]$ParentPid,
    [Parameter(Mandatory=$true)][string]$AppPath,
    [Parameter(Mandatory=$true)][string]$Package,
    [Parameter(Mandatory=$true)][ValidatePattern('^[a-f0-9]{64}$')][string]$Sha256
)
$ErrorActionPreference = 'Stop'
$stage = Split-Path -Parent $Package
$result = Join-Path $stage 'result.txt'
try {
    $parent = Get-Process -Id $ParentPid
    if ($parent.Path -ine $AppPath) { throw 'The parent application path changed.' }
    # Open the actual process handle BEFORE the ready handshake. PID reuse
    # cannot make us wait for or terminate an unrelated process afterwards.
    $null = $parent.Handle
    [IO.File]::WriteAllText((Join-Path $stage 'ready'), 'ready')
    $parent.WaitForExit()
    if ((Get-FileHash -LiteralPath $Package -Algorithm SHA256).Hash -ine $Sha256) {
        throw 'The verified MSI changed before installation.'
    }
    $log = Join-Path $stage 'msiexec.log'
    # No elevation or machine restart. The MSI retains its per-user scope and
    # MajorUpgrade/rollback behavior. Keep the log and package on failure.
    $arguments = '/i "{0}" /passive /norestart REBOOT=ReallySuppress /l*v "{1}"' -f $Package,$log
    $installer = Start-Process -FilePath "$env:SystemRoot\System32\msiexec.exe" -ArgumentList $arguments -Wait -PassThru
    if ($installer.ExitCode -notin @(0,3010)) { throw "Windows Installer returned $($installer.ExitCode). See $log" }
    [IO.File]::WriteAllText($result, "Installed; Windows Installer exit $($installer.ExitCode)")
    Start-Process -FilePath $AppPath
} catch {
    [IO.File]::WriteAllText($result, $_.Exception.Message)
    # The parent has exited; show the failure instead of silently losing it.
    Add-Type -AssemblyName System.Windows.Forms
    [System.Windows.Forms.MessageBox]::Show($_.Exception.Message, 'QBZ update', 'OK', 'Error') | Out-Null
    exit 1
}
