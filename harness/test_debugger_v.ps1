# Test debugger protocol verbs 'v' (variables) and 'p' (evaluate)
$p = Get-Process -Name PS7_ISE | Select-Object -First 1
if (-not $p) { Write-Error "PS7_ISE not running"; exit 1 }

$handles = Get-ChildItem "\\.\pipe\" -ErrorAction SilentlyContinue | Where-Object { $_.Name -match "ps7ise.*stdin" }
if (-not $handles) { Write-Error "Could not find engine stdin pipe"; exit 1 }
$stdinPath = $handles[0].FullName
Write-Host "Found engine stdin: $stdinPath"

$script = @"
function Get-Total {
    param([int[]]`$Numbers)
    `$sum = 0
    foreach (`$n in `$Numbers) {
        `$sum = `$sum + `$n
    }
    return `$sum
}
`$values = 1..4
`$result = Get-Total -Numbers `$values
Write-Output ("TOTAL=" + `$result)
"@

$scriptFile = [IO.Path]::GetTempPath() + "test_dbg_v.ps1"
[IO.File]::WriteAllText($scriptFile, $script)

$stdin = [IO.File]::OpenWrite($stdinPath)
$writer = [IO.StreamWriter]::new($stdin)
$writer.AutoFlush = $true

$writer.WriteLine("D b + 5 " + $scriptFile)
Start-Sleep -Seconds 1
$writer.WriteLine("L & '" + $scriptFile + "'")
Start-Sleep -Seconds 3

# Execution should be stopped at line 5 ($sum = $sum + $n)
# Test variables ('v') and evaluate ('p')
$writer.WriteLine("D v 0")
Write-Host "Sent D v 0 to list variables"
Start-Sleep -Seconds 1

$writer.WriteLine("D p `$sum")
Write-Host "Sent D p `$sum to evaluate sum"
Start-Sleep -Seconds 1

$writer.WriteLine("D c")
Start-Sleep -Seconds 1
$writer.WriteLine("D b c")

$writer.Close()
$stdin.Close()
Write-Host "Test complete"
