# Manual smoke run of the Docker Sandbox provider (#40) against a real Docker
# account: creates one small Blank sandbox, runs commands, installs Zeron,
# checks the watchdog, stops/starts it and deletes it (a few cents).
#
# Needs `sbx` signed in on this Windows machine (`sbx login`). It borrows sbx's
# current access token from Windows Credential Manager for the run, passes it
# in ZERON_DOCKER_SMOKE_TOKEN and never prints it. It doesn't use sbx's refresh
# token: refresh tokens rotate, so using it would sign sbx out.
#
#   powershell -NoProfile -File scripts\smoke-docker-sandbox.ps1 [-Cargo "<cargo wrapper>"]
#
# On a shared laptop pass the build-slot wrapper, for example
#   -Cargo "& '<path to>\cargo-slot.ps1' cargo"
param([string]$Cargo = "cargo")
$ErrorActionPreference = "Stop"

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class SbxCred {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct CREDENTIAL {
        public int Flags; public int Type; public string TargetName; public string Comment;
        public long LastWritten; public int CredentialBlobSize; public IntPtr CredentialBlob;
        public int Persist; public int AttributeCount; public IntPtr Attributes;
        public string TargetAlias; public string UserName;
    }
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool CredRead(string target, int type, int flags, out IntPtr cred);
    [DllImport("advapi32.dll")] static extern void CredFree(IntPtr cred);
    public static string Read(string target) {
        IntPtr p;
        if (!CredRead(target, 1, 0, out p)) return null;
        try {
            var c = (CREDENTIAL)Marshal.PtrToStructure(p, typeof(CREDENTIAL));
            var bytes = new byte[c.CredentialBlobSize];
            Marshal.Copy(c.CredentialBlob, bytes, 0, bytes.Length);
            return Encoding.Unicode.GetString(bytes);
        } finally { CredFree(p); }
    }
}
"@

$sbx = Join-Path $env:LOCALAPPDATA "DockerSandboxes\bin\sbx.exe"
if (-not (Test-Path $sbx)) { throw "sbx isn't installed (winget install -h Docker.sbx)" }
# Let sbx refresh its own sign-in first, so the token has its full 15 minutes.
& $sbx --no-input --cloud ls *> $null
if ($LASTEXITCODE -ne 0) { throw "sbx isn't signed in (sbx login)" }

$base = (cmdkey /list | Select-String "com\.docker\.sandboxes-auth:sandboxes-auth:docker/auth/hub/[^:\s]+" |
    Select-Object -First 1).Matches.Value
if (-not $base) { throw "no sbx sign-in in Credential Manager" }
$blob = ""
for ($i = 0; ; $i++) {
    $part = [SbxCred]::Read("${base}:chunk:$i")
    if ($null -eq $part) { break }
    $blob += $part
}
if (-not $blob) { $blob = [SbxCred]::Read($base) }
$token = ($blob | ConvertFrom-Json).access_token
if (-not $token) { throw "sbx's sign-in has no access token" }

$env:ZERON_DOCKER_SMOKE_TOKEN = $token
try {
    $command = "$Cargo test --locked -p zeron-engine --lib sandboxes::docker::smoke -- --ignored --nocapture"
    Invoke-Expression $command
    exit $LASTEXITCODE
} finally {
    Remove-Item Env:ZERON_DOCKER_SMOKE_TOKEN -ErrorAction SilentlyContinue
}
