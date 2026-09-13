$ErrorActionPreference = 'Stop'
$artifacts = cargo test --locked -p kraai-sandbox --no-run --message-format=json |
    ForEach-Object { $_ | ConvertFrom-Json } |
    Where-Object { $_.reason -eq 'compiler-artifact' -and $_.profile.test -and $_.executable }
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$name = 'KraaiTest' + [guid]::NewGuid().ToString('N').Substring(0, 8)
$password = ConvertTo-SecureString ('Kraai!' + [guid]::NewGuid().ToString('N')) -AsPlainText -Force
$user = New-LocalUser -Name $name -Password $password -AccountNeverExpires
$directory = Join-Path $env:PUBLIC $name
try {
    $group = Get-LocalGroup -SID 'S-1-5-32-545'
    Add-LocalGroupMember -Group $group -Member $user
    New-Item -ItemType Directory -Path $directory | Out-Null
    & icacls.exe $directory /grant ('*' + $user.SID.Value + ':(OI)(CI)F') | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Cannot grant test user access to fixtures' }
    $commands = @(
        '$ErrorActionPreference = ''Stop''',
        '$identity = [Security.Principal.WindowsIdentity]::GetCurrent()',
        '$principal = [Security.Principal.WindowsPrincipal]::new($identity)',
        'if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw ''Tests must run without administrator rights'' }',
        '$env:TEMP = Join-Path $PSScriptRoot ''temp''',
        '$env:TMP = $env:TEMP',
        'New-Item -ItemType Directory -Path $env:TEMP | Out-Null'
    )
    foreach ($target in @('kraai_sandbox', 'windows', 'capabilities')) {
        $artifact = @($artifacts | Where-Object { $_.target.name -eq $target })
        if ($artifact.Count -ne 1) { throw "Expected one executable for $target" }
        Copy-Item $artifact[0].executable (Join-Path $directory "$target.exe")
        $arguments = if ($target -eq 'kraai_sandbox') { 'windows_helper::tests --nocapture' }
            elseif ($target -eq 'capabilities') { '--skip host_read --skip host_write --nocapture' }
            else { '--nocapture' }
        $commands += '& (Join-Path $PSScriptRoot ''' + $target + '.exe'') ' + $arguments
        $commands += 'if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }'
    }
    $commands += 'exit 0'
    $script = Join-Path $directory 'test.ps1'
    $commands | Set-Content $script
    & icacls.exe $directory /grant ('*' + $user.SID.Value + ':(OI)(CI)F') /T | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Cannot grant test user access to copied executables' }
    $credential = [pscredential]::new("$env:COMPUTERNAME\$name", $password)
    $process = Start-Process powershell.exe -Credential $credential -LoadUserProfile -Wait -PassThru `
        -WorkingDirectory $directory -ArgumentList @('-NoProfile', '-NonInteractive', '-File', ('"' + $script + '"')) `
        -RedirectStandardOutput (Join-Path $directory 'stdout') -RedirectStandardError (Join-Path $directory 'stderr')
    Get-Content (Join-Path $directory 'stdout')
    Get-Content (Join-Path $directory 'stderr')
    if ($process.ExitCode -ne 0) { throw "Standard user tests failed with $($process.ExitCode)" }
} finally {
    Remove-LocalUser -Name $name
    if (Test-Path $directory) { Remove-Item -Recurse -Force $directory }
}
