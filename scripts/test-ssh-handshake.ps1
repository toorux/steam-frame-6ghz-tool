$ErrorActionPreference = 'Stop'
$temp = (Resolve-Path -LiteralPath ([IO.Path]::GetTempPath())).Path
$name = "frame-ssh-$([guid]::NewGuid())"
$root = New-Item -ItemType Directory -Path (Join-Path $temp $name)
$server = $null
$log = Join-Path $root.FullName 'sshd.log'
try {
    $archive = Join-Path $root.FullName 'OpenSSH-Win64.zip'
    Invoke-WebRequest -Uri 'https://github.com/PowerShell/Win32-OpenSSH/releases/download/10.4.0.0p1-Preview/OpenSSH-Win64.zip' -OutFile $archive
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne 'b67ff8becd203502190ec42ac7787c24c2cd3325ed7700485fe7a1c83d2a7957') {
        throw 'Unexpected OpenSSH fixture archive checksum.'
    }
    Expand-Archive -LiteralPath $archive -DestinationPath $root.FullName
    $openssh = Join-Path $root.FullName 'OpenSSH-Win64'
    $key = Join-Path $root.FullName 'host_ed25519'
    & (Join-Path $openssh 'ssh-keygen.exe') -q -t ed25519 -N '' -f $key
    if ($LASTEXITCODE -ne 0) { throw 'Could not generate the fixture host key.' }
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    icacls $key /inheritance:r /grant:r "*${sid}:F" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Could not restrict permissions on the fixture host key.' }
    $keyInfo = & (Join-Path $openssh 'ssh-keygen.exe') -E sha256 -lf "$key.pub"
    if ($LASTEXITCODE -ne 0 -or $keyInfo -notmatch '\s(SHA256:\S+)') {
        throw 'Could not read the fixture host key fingerprint.'
    }
    $env:FRAME_SSH_TEST_FINGERPRINT = $Matches[1]

    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    try { $port = $listener.LocalEndpoint.Port } finally { $listener.Stop() }
    $config = Join-Path $root.FullName 'sshd_config'
    @"
ListenAddress 127.0.0.1
Port $port
HostKey "$($key.Replace('\', '/'))"
KexAlgorithms curve25519-sha256
HostKeyAlgorithms ssh-ed25519
PasswordAuthentication no
PubkeyAuthentication no
KbdInteractiveAuthentication no
"@ | Set-Content -LiteralPath $config -Encoding ascii

    # Debug mode serves one connection, so wait for the listening log without opening a socket.
    $server = Start-Process -FilePath (Join-Path $openssh 'sshd.exe') -ArgumentList "-D -ddd -e -f `"$config`"" -RedirectStandardError $log -WindowStyle Hidden -PassThru
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        if ($server.HasExited -or [DateTime]::UtcNow -gt $deadline) { throw 'The OpenSSH fixture failed to start.' }
        Start-Sleep -Milliseconds 100
    } until ((Get-Content -LiteralPath $log -Raw) -match "Server listening on 127\.0\.0\.1 port $port\.")

    $env:FRAME_SSH_TEST_ADDRESS = "127.0.0.1:$port"
    cargo test --locked frame::tests::openssh_modern_handshake -- --ignored --exact
    if ($LASTEXITCODE -ne 0) { throw 'The OpenSSH handshake test failed.' }
} catch {
    if (Test-Path -LiteralPath $log) { Get-Content -LiteralPath $log | Write-Host }
    throw
} finally {
    if ($server -and !$server.WaitForExit(2000)) {
        Stop-Process -InputObject $server -ErrorAction SilentlyContinue
        [void]$server.WaitForExit(2000)
    }
    Remove-Item -LiteralPath Env:FRAME_SSH_TEST_ADDRESS,Env:FRAME_SSH_TEST_FINGERPRINT -ErrorAction SilentlyContinue
    $resolved = (Resolve-Path -LiteralPath $root.FullName).Path
    if ($resolved -eq (Join-Path $temp $name)) {
        # Windows can still hold an OpenSSH DLL briefly after the handshake ends.
        for ($attempt = 1; $attempt -le 8; $attempt++) {
            try {
                Remove-Item -LiteralPath $resolved -Recurse -Force -ErrorAction Stop
                break
            } catch {
                if ($attempt -eq 8) {
                    Write-Warning "Could not remove the OpenSSH fixture directory: $_"
                } else {
                    Start-Sleep -Milliseconds 500
                }
            }
        }
    }
}
