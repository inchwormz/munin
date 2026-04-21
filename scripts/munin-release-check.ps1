param(
    [string]$MuninBin = "",
    [string]$RepoRoot = ""
)

$ErrorActionPreference = "Stop"

if (-not $RepoRoot) {
    $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
}

if (-not $MuninBin) {
    $debugBin = Join-Path $RepoRoot "target\debug\munin.exe"
    if (Test-Path $debugBin) {
        $MuninBin = $debugBin
    } else {
        $cmd = Get-Command munin -ErrorAction SilentlyContinue
        if ($cmd) {
            $MuninBin = $cmd.Source
        }
    }
}

if ($MuninBin -and (Test-Path $MuninBin)) {
    $MuninBin = (Resolve-Path $MuninBin).Path
}

$results = New-Object System.Collections.Generic.List[object]

function Add-Result {
    param(
        [string]$Name,
        [string]$Status,
        [string]$Detail
    )
    $script:results.Add([pscustomobject]@{
        Name = $Name
        Status = $Status
        Detail = $Detail
    }) | Out-Null
}

function Run-Munin {
    param(
        [string]$Name,
        [string[]]$CliArgs,
        [string]$WorkingDirectory,
        [scriptblock]$Judge
    )

    Push-Location $WorkingDirectory
    try {
        $oldErrorActionPreference = $ErrorActionPreference
        $ErrorActionPreference = "Continue"
        $output = (& $MuninBin @CliArgs 2>&1 | Out-String)
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $oldErrorActionPreference
        Pop-Location
    }

    & $Judge $Name $exitCode $output ""
}

function First-Line {
    param([string]$Text)
    $line = ($Text -split "`r?`n" | Where-Object { $_.Trim() } | Select-Object -First 1)
    if ($line) { return $line.Trim() }
    return ""
}

Write-Output "Munin Release Check"
Write-Output "-------------------"

if (-not $MuninBin -or -not (Test-Path $MuninBin)) {
    Add-Result "Binary" "FAIL" "Munin binary was not found. Build with 'cargo build --bin munin' or install Munin first."
} else {
    Add-Result "Binary" "PASS" $MuninBin

    Run-Munin "Install contract from repo" @("install", "--check-resolvable") $RepoRoot {
        param($Name, $ExitCode, $Stdout, $Stderr)
        if ($ExitCode -eq 0 -and $Stdout -match "resolver, skill, and fixture checks passed") {
            Add-Result $Name "PASS" (First-Line $Stdout)
        } else {
            Add-Result $Name "FAIL" (First-Line ($Stderr + "`n" + $Stdout))
        }
    }

    $tempCwd = Join-Path ([System.IO.Path]::GetTempPath()) ("munin-release-check-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path $tempCwd | Out-Null
    try {
        Run-Munin "Install contract from temp folder" @("install", "--check-resolvable") $tempCwd {
            param($Name, $ExitCode, $Stdout, $Stderr)
            if ($ExitCode -eq 0 -and $Stdout -match "resolver, skill, and fixture checks passed") {
                Add-Result $Name "PASS" (First-Line $Stdout)
            } else {
                Add-Result $Name "FAIL" (First-Line ($Stderr + "`n" + $Stdout))
            }
        }
    } finally {
        Remove-Item -LiteralPath $tempCwd -Recurse -Force -ErrorAction SilentlyContinue
    }

    Run-Munin "Memory health" @("doctor", "--scope", "user", "--format", "text") $RepoRoot {
        param($Name, $ExitCode, $Stdout, $Stderr)
        if ($ExitCode -ne 0) {
            Add-Result $Name "FAIL" (First-Line ($Stderr + "`n" + $Stdout))
        } elseif ($Stdout -match "Status: warn") {
            Add-Result $Name "WARN" "Doctor reports warn. Check the recommended permanent fix."
        } elseif ($Stdout -match "Status: pass|Status: ok|Status: healthy") {
            Add-Result $Name "PASS" (First-Line $Stdout)
        } else {
            Add-Result $Name "WARN" "Doctor ran, but status was not recognised."
        }
    }

    Run-Munin "Promotion proof" @("prove", "--format", "text") $RepoRoot {
        param($Name, $ExitCode, $Stdout, $Stderr)
        if ($ExitCode -ne 0) {
            Add-Result $Name "FAIL" (First-Line ($Stderr + "`n" + $Stdout))
        } elseif ($Stdout -match "Decision: .*passed|Strict promotion gate passed") {
            Add-Result $Name "PASS" "Promotion proof passed."
        } elseif ($Stdout -match "blocked|missing") {
            Add-Result $Name "WARN" "Promotion proof is blocked or missing required rows."
        } else {
            Add-Result $Name "WARN" "Proof ran, but decision was not recognised."
        }
    }

    Run-Munin "Resolver routing" @("resolve", "--format", "text", "what", "keeps", "going", "wrong") $RepoRoot {
        param($Name, $ExitCode, $Stdout, $Stderr)
        if ($ExitCode -eq 0 -and $Stdout -match "Route: friction") {
            Add-Result $Name "PASS" "Plain-English friction question routes correctly."
        } else {
            Add-Result $Name "FAIL" (First-Line ($Stderr + "`n" + $Stdout))
        }
    }
}

foreach ($result in $results) {
    Write-Output ("[{0}] {1} - {2}" -f $result.Status, $result.Name, $result.Detail)
}

if ($results.Status -contains "FAIL") {
    Write-Output "Next action: fix the FAIL items before trusting this release."
    exit 1
}

if ($results.Status -contains "WARN") {
    Write-Output "Next action: release is mechanically usable, but review the WARN items."
    exit 0
}

Write-Output "Next action: release check is green."
exit 0
