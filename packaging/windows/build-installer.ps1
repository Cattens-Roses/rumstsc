[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidatePattern('^[0-9][0-9A-Za-z.+_-]*$')][string]$Version,
    [string]$Binary,
    [string]$OutputDirectory = 'dist',
    [string]$Compiler
)
$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
if (-not $Binary) { $Binary = Join-Path $repoRoot 'target/release/rumstsc.exe' }
$Binary = (Resolve-Path -LiteralPath $Binary).Path
if (-not [IO.Path]::IsPathRooted($OutputDirectory)) {
    $OutputDirectory = Join-Path $repoRoot $OutputDirectory
}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
if (-not $Compiler) {
    $command = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($command) { $Compiler = $command.Source }
    else { $Compiler = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6/ISCC.exe' }
}
if (-not (Test-Path -LiteralPath $Compiler -PathType Leaf)) {
    throw 'Inno Setup 6 is required. Install it or pass -Compiler with the path to ISCC.exe.'
}
# Windows resource versions contain 3-4 numeric components, unlike release names.
$numeric = ($Version -split '[+-]')[0]
if ($numeric -notmatch '^\d+\.\d+\.\d+(\.\d+)?$') { throw "Invalid numeric version: $numeric" }
$parts = @($numeric -split '\.')
if ($parts.Count -eq 3) { $parts += '0' }
foreach ($part in $parts) {
    if ([uint32]::Parse($part) -gt 65535) { throw 'Windows version components must be at most 65535.' }
}
$numeric = $parts -join '.'
$fileVersion = $Version -replace '[^0-9A-Za-z._-]', '-'
$filename = "rumstsc-$fileVersion-windows-x64-setup"
$compilerArguments = @(
    '/Qp', "/DRepoRoot=$repoRoot", "/DBinaryPath=$Binary",
    "/DAppVersion=$Version", "/DNumericVersion=$numeric",
    "/O$OutputDirectory", "/F$filename", (Join-Path $PSScriptRoot 'rumstsc.iss')
)
& $Compiler @compilerArguments
if ($LASTEXITCODE -ne 0) { throw "Installer compilation failed with exit code $LASTEXITCODE" }
$installer = Join-Path $OutputDirectory "$filename.exe"
if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) { throw 'Installer output is missing.' }
Get-Item -LiteralPath $installer
