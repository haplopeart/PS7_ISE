# Test debugger protocol by writing to the engine's stdin
# The engine reads from stdin, so we can send D commands

$p = Get-Process -Name PS7_ISE | Select-Object -First 1
if (-not $p) { Write-Error "PS7_ISE not running"; exit 1 }

# Get the stdin handle - we need to open it
# The engine's stdin is a pipe we can write to
$stdinPath = "\\.\pipe\ps7ise-$($p.Id)-stdin"
if (-not (Test-Path $stdinPath)) {
    # Find the actual pipe name
    $handles = Get-ChildItem "\\.\pipe\" -ErrorAction SilentlyContinue | Where-Object { $_.Name -match "ps7ise.*stdin" }
    if ($handles) { $stdinPath = $handles[0].FullName }
}

if (-not (Test-Path $stdinPath)) {
    Write-Error "Could not find engine stdin pipe"
    exit 1
}

Write-Host "Found engine stdin: $stdinPath"

# Send a test script to the prompt
$script = @"
function Get-Total {
    param([int[]]$Numbers)
    \$sum = 0
    foreach (\$n in \$Numbers) {
        \$sum = \$sum + \$n
    }
    return \$sum
}
\$values = 1..4
\$result = Get-Total -Numbers \$values
Write-Output ("TOTAL=" + \$result)
"@

# Write the script to a file
$scriptFile = [IO.Path]::GetTempPath() + "test_dbg.ps1"
[IO.File]::WriteAllText($scriptFile, $script)
Write-Host "Script written to $scriptFile"

# Send the script to run
$stdin = [IO.File]::OpenWrite($stdinPath)
$writer = [IO.StreamWriter]::new($stdin)
$writer.AutoFlush = $true

# First, run the script
$writer.WriteLine("L & '" + $scriptFile + "'")
Write-Host "Sent L command to run script"
Start-Sleep -Seconds 3

# Now test debug protocol - set a breakpoint
$writer.WriteLine("D b + 5 " + $scriptFile)
Write-Host "Sent D b + to set breakpoint at line 5"
Start-Sleep -Seconds 1

# List breakpoints
$writer.WriteLine("D b l")
Write-Host "Sent D b l to list breakpoints"
Start-Sleep -Seconds 1

# Run the script again to hit breakpoint
$writer.WriteLine("L & '" + $scriptFile + "'")
Write-Host "Sent L command to run script again"
Start-Sleep -Seconds 3

# Continue
$writer.WriteLine("D c")
Write-Host "Sent D c to continue"
Start-Sleep -Seconds 2

# Clear breakpoints
$writer.WriteLine("D b c")
Write-Host "Sent D b c to clear breakpoints"

$writer.Close()
$stdin.Close()
Write-Host "Test complete"