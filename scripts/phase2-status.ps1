#!/usr/bin/env pwsh
# Windows 版阶段二进度跟踪 / 回退检测（scripts/phase2-status.sh 的 PowerShell 等价物）。
#
#   pwsh -File scripts/phase2-status.ps1             三级验证 + 全量回归 + 与基线逐套件对比
#   pwsh -File scripts/phase2-status.ps1 -Quick      只跑三级，跳过全量
#   pwsh -File scripts/phase2-status.ps1 -Update     跑完刷新 docs/phase2-status.tsv 与文档数字
#
# 退出码：0 = 无逐套件回退；1 = 有回退或验证失败；2 = 环境问题；3 = runner 构建失败。
#
# 与 .sh 的差异（都是 Windows 特有，语义保持一致）：
#   * 没有 ulimit -v —— 分片独立进程 + runner 自带的 TEST262_MEMORY_MB 护栏就是全部保护；
#   * runner 直接从 target/release/deps/test262_runner-*.exe 里按时间戳取最新的，
#     原生排除 MSVC 的 .pdb / .d（.sh 在 MSYS 下曾被 .pdb 坑过）；
#   * 只串行分片；即使设了 TEST262_JOBS 也忽略（实测并发更慢且护栏分类会漂）；
#   * 不依赖 bash / awk / perl：TSV 比较与 README / PROJECT 的数字刷新都在这里做。
#
# 环境变量与 .sh 相同：TEST262_TIMEOUT_MS / TEST262_STEP_LIMIT / TEST262_MEMORY_MB /
# TEST262_CHUNKS / TEST262_HARD_TIMEOUT / TEST262_JOBS（最后一个仅提示）。

[CmdletBinding()]
param(
    [switch]$Update,
    [switch]$Quick
)

# 不能用 Stop：PowerShell 7 会把 cargo 写到 stderr 的**编译警告**当终止错误抛出。
# 这里的失败判据是显式的 `$LASTEXITCODE` / `test result` 检查与 `throw`。
$ErrorActionPreference = 'Continue'
$PSNativeCommandUseErrorActionPreference = $false
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $root
$snap = 'docs/phase2-status.tsv'
$expectedSkipped = 8577

function Get-EnvInt {
    param([string]$Name, [int]$Default)
    $raw = [Environment]::GetEnvironmentVariable($Name)
    if ([string]::IsNullOrWhiteSpace($raw) -or $raw -notmatch '^\d+$') { return $Default }
    return [int]$raw
}

function Convert-CargoOutput {
    # `2>&1` 把原生命令的 stderr 包成 ErrorRecord，直接 ToString() 只会得到类型名。
    param([object[]]$Items)
    foreach ($item in $Items) {
        if ($item -is [System.Management.Automation.ErrorRecord]) { $item.Exception.Message }
        else { $item.ToString() }
    }
}

function Invoke-CargoTest {
    param([string]$Label, [string[]]$Arguments)
    Write-Host ''
    Write-Host "== $Label =="
    $output = & cargo @Arguments 2>&1
    $code = $LASTEXITCODE
    $text = @(Convert-CargoOutput @($output))
    $text | Where-Object { $_ -match '^test result' } | ForEach-Object { Write-Host $_ }
    if ($code -ne 0) {
        $text | Select-Object -Last 25 | ForEach-Object { Write-Host $_ }
        throw "cargo 命令失败（退出码 $code）：$Label"
    }
    if (-not ($text | Where-Object { $_ -match '^test result: ok' })) {
        throw "没有看到成功的 test result：$Label"
    }
}

function Write-Snapshot {
    param(
        [hashtable]$Rows,
        [string[]]$Names,
        [int]$GuardTimeout,
        [int]$GuardStep,
        [int]$GuardMemory
    )
    $header = @(
        '# 阶段二基线快照 —— 由 phase2-status 脚本生成'
        '# 格式：suite<TAB>passed<TAB>skipped<TAB>failed'
        ("# 最近更新：{0}" -f (Get-Date -Format 'yyyy-MM-dd'))
        ("# guards: timeout {0} step-limit {1} memory {2} (TEST262_TIMEOUT_MS={3} TEST262_STEP_LIMIT={4} TEST262_MEMORY_MB={5})" -f
            $GuardTimeout, $GuardStep, $GuardMemory, $script:timeoutMs, $script:stepLimit, $script:memoryMb)
    )
    if (Test-Path -LiteralPath $snap) {
        $first = @(Get-Content -LiteralPath $snap -TotalCount 1)
        if ($first.Count -gt 0 -and $first[0] -match '^#') { $header[0] = $first[0] }
    }
    $data = foreach ($name in $Names) {
        $row = $Rows[$name]
        "{0}`t{1}`t{2}`t{3}" -f $name, $row.Passed, $row.Skipped, $row.Failed
    }
    $content = (($header + @($data)) -join "`n") + "`n"
    [IO.File]::WriteAllText((Join-Path $root $snap), $content, (New-Object Text.UTF8Encoding($false)))
}

function Update-DocNumbers {
    # 等价 scripts/doc-numbers.sh：README / PROJECT 的通过率一律从快照生成。
    $p = 0; $s = 0; $f = 0
    $updated = ''
    $budget = '护栏预算未记录'
    foreach ($line in [IO.File]::ReadAllLines((Join-Path $root $snap))) {
        if ($line -match '^#\s*最近更新：\s*(.+)') { $updated = $Matches[1].Trim() }
        if ($line -match '^#\s*guards:.*TEST262_MEMORY_MB=(\d+)') { $budget = "TEST262_MEMORY_MB=$($Matches[1])" }
        if ($line -match '^#') { continue }
        $parts = $line -split "`t"
        if ($parts.Count -ge 4) {
            $p += [int]$parts[1]; $s += [int]$parts[2]; $f += [int]$parts[3]
        }
    }
    $exec = $p + $f
    if ($exec -eq 0) { throw "$snap 里没有数据" }
    $rate = '{0:F2}' -f (100.0 * $p / $exec)
    $text = "**$rate%**（$p / 执行 $exec；跳过 $s；$updated 基线，$budget）"
    $pattern = '<!-- doc-numbers -->.*?<!-- /doc-numbers -->'
    $replacement = "<!-- doc-numbers -->$text<!-- /doc-numbers -->"
    foreach ($doc in @('README.md', 'docs/PROJECT.md')) {
        $path = Join-Path $root $doc
        $src = [IO.File]::ReadAllText($path)
        if (-not [regex]::IsMatch($src, $pattern, [Text.RegularExpressions.RegexOptions]::Singleline)) {
            throw "$doc 缺少 doc-numbers 标记"
        }
        $next = [regex]::Replace($src, $pattern, $replacement, [Text.RegularExpressions.RegexOptions]::Singleline)
        [IO.File]::WriteAllText($path, $next, (New-Object Text.UTF8Encoding($false)))
        Write-Host "  已更新 $doc"
    }
    Write-Host "  test262 通过率：$text"
}

$script:timeoutMs   = Get-EnvInt 'TEST262_TIMEOUT_MS' 15000
$script:stepLimit   = Get-EnvInt 'TEST262_STEP_LIMIT' 0
$script:memoryMb    = Get-EnvInt 'TEST262_MEMORY_MB' 384
$chunks             = Get-EnvInt 'TEST262_CHUNKS' 4
$hardTimeout        = Get-EnvInt 'TEST262_HARD_TIMEOUT' 1200
$jobs               = Get-EnvInt 'TEST262_JOBS' 1
if ($chunks -lt 1) { $chunks = 1 }
if ($jobs -gt 1) {
    Write-Host "TEST262_JOBS=$jobs：Windows 版只串行跑分片（实测并发更慢且护栏分类会漂）。"
}

$env:TEST262_TIMEOUT_MS  = "$script:timeoutMs"
$env:TEST262_STEP_LIMIT  = "$script:stepLimit"
$env:TEST262_MEMORY_MB   = "$script:memoryMb"
$env:TEST262_CHUNKS      = "$chunks"

# ── 1/2/3. 构建 + 三级验证 ──────────────────────────────────────────────────
Write-Host '== 构建 =='
$buildOut = & cargo build --release 2>&1
$buildCode = $LASTEXITCODE
$buildText = @(Convert-CargoOutput @($buildOut))
$buildText | Select-Object -Last 3 | ForEach-Object { Write-Host $_ }
if ($buildCode -ne 0) { exit 1 }

Invoke-CargoTest '单元测试' @('test', '--release', '--lib')
Invoke-CargoTest 'feature 集成测试' @('test', '--release', '--test', 'features')
Invoke-CargoTest '护栏测试（超时 / 步数 / 堆预算）' @('test', '--release', '--test', 'guards')

if ($Quick) {
    Write-Host ''
    Write-Host '-Quick：跳过全量回归。'
    exit 0
}

# ── 4. 全量 test262 ────────────────────────────────────────────────────────
# 必须先确认 runner 构建成功，否则会安静地挑到一个旧二进制，跑出"看起来很漂亮"的数字。
Write-Host ''
Write-Host '== 构建 test262_runner =='
$runnerBuild = & cargo test --release --test test262_runner --no-run 2>&1
if ($LASTEXITCODE -ne 0) {
    @(Convert-CargoOutput @($runnerBuild)) | Select-Object -Last 20 | ForEach-Object { Write-Host $_ }
    Write-Host 'test262_runner 构建失败 —— 拒绝用旧二进制跑全量。'
    exit 3
}
$runner = Get-ChildItem -LiteralPath 'target/release/deps' -Filter 'test262_runner-*.exe' -File |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($null -eq $runner) {
    Write-Host '找不到 test262_runner 二进制（先 cargo test --release --test test262_runner --no-run）。'
    exit 1
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ('biujs-phase2-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
$exitCode = 0

try {
    Write-Host ''
    Write-Host "== 全量 test262（分 $chunks 片串行跑；每片硬超时 ${hardTimeout}s）=="
    Write-Host ("   护栏：单例 {0}ms / {1} 步 / 堆 {2}MB" -f $script:timeoutMs, $script:stepLimit, $script:memoryMb)
    $wall = [Diagnostics.Stopwatch]::StartNew()
    $chunkFiles = [System.Collections.Generic.List[string]]::new()

    for ($i = 0; $i -lt $chunks; $i++) {
        $outFile = Join-Path $tmp "full-chunk-$i.txt"
        $errFile = Join-Path $tmp "full-chunk-$i.err.txt"
        $env:TEST262_FAILURES = '0'
        $env:TEST262_CHUNK_INDEX = "$i"
        $proc = Start-Process -FilePath $runner.FullName -ArgumentList '--nocapture' -PassThru -NoNewWindow `
            -RedirectStandardOutput $outFile -RedirectStandardError $errFile
        if (-not $proc.WaitForExit($hardTimeout * 1000)) {
            $proc.Kill()
            Write-Host "第 $i 片超过硬超时 ${hardTimeout}s 被中止。"
            exit 1
        }
        $proc.WaitForExit()
        # Windows 上 `Start-Process -PassThru` 的对象在进程退出后常常读不到
        # `ExitCode`（值为 $null）：那不代表失败，只按摘要完整性（`TOTAL`）判分片。
        $chunkExit = if ($null -ne $proc.ExitCode) { [int]$proc.ExitCode } else { 0 }
        $lines = @(Get-Content -LiteralPath $outFile -ErrorAction SilentlyContinue)
        $errLines = @(Get-Content -LiteralPath $errFile -ErrorAction SilentlyContinue)
        [string[]]$all = @($lines) + @($errLines)
        [IO.File]::WriteAllLines($outFile, $all, (New-Object Text.UTF8Encoding($false)))
        $chunkFiles.Add($outFile)

        if (-not ($all | Where-Object { $_ -match '^\s*TOTAL\s+\d+' })) {
            Write-Host "第 $i 片没有跑完（进程可能被内存上限打断或崩溃）。最后 20 行："
            $all | Select-Object -Last 20 | ForEach-Object { Write-Host $_ }
            exit 1
        }
        if ($chunkExit -ne 0) {
            Write-Host "第 $i 片的 test262_runner 退出码 $chunkExit，但摘要已生成 —— 请检查异常。"
        }
    }
    $wall.Stop()

    # ── 5. 汇总（表头到分隔线之间是逐套件结果；TOTAL / guards 每片各一行）────
    $passed = 0; $skipped = 0; $failed = 0
    $gTimeout = 0; $gStep = 0; $gMemory = 0
    $newRows = @{}
    $orderedNames = [System.Collections.Generic.List[string]]::new()

    foreach ($file in $chunkFiles) {
        $inTable = $false
        foreach ($line in [IO.File]::ReadAllLines($file)) {
            if ($line -match '^suite\s+passed\s+skipped\s+failed') { $inTable = $true; continue }
            if ($line -match '^-{10,}') { $inTable = $false; continue }
            if ($inTable -and $line -match '^\s*(\S+)\s+(\d+)\s+(\d+)\s+(\d+)\s*$') {
                $name = $Matches[1]
                if (-not $newRows.ContainsKey($name)) { $orderedNames.Add($name) }
                $newRows[$name] = [pscustomobject]@{
                    Passed = [int]$Matches[2]; Skipped = [int]$Matches[3]; Failed = [int]$Matches[4]
                }
                continue
            }
            if ($line -match '^\s*TOTAL\s+(\d+)\s+(\d+)\s+(\d+)') {
                $passed += [int]$Matches[1]; $skipped += [int]$Matches[2]; $failed += [int]$Matches[3]
                continue
            }
            if ($line -match 'guards:\s*timeout\s+(\d+),\s*step-limit\s+(\d+),\s*memory\s+(\d+)') {
                $gTimeout += [int]$Matches[1]; $gStep += [int]$Matches[2]; $gMemory += [int]$Matches[3]
            }
        }
    }

    $executed = $passed + $failed
    $rate = if ($executed -gt 0) { '{0:F2}' -f (100.0 * $passed / $executed) } else { '0.00' }
    Write-Host ''
    Write-Host ('─' * 56)
    Write-Host ("  TOTAL                                              {0,7} {1,7} {2,7}" -f $passed, $skipped, $failed)
    Write-Host "  pass rate over executed tests: ${rate}% ($passed / $executed)"
    Write-Host "  guards: timeout $gTimeout, step-limit $gStep, memory $gMemory   (TEST262_TIMEOUT_MS / TEST262_STEP_LIMIT / TEST262_MEMORY_MB, 0 = off)"
    Write-Host ("  wall clock: {0:F1}s" -f $wall.Elapsed.TotalSeconds)
    Write-Host "  执行 $executed = 通过 $passed + 失败 $failed；跳过 $skipped"
    if ($skipped -ne $expectedSkipped) {
        Write-Host "  !! 跳过数从 $expectedSkipped 变为 $skipped —— 跳过表被改动了（请确认是有意的，并更新 EXPECTED_SKIPPED）。"
    }
    Write-Host ('─' * 56)

    # ── 6. 与基线逐套件比较 ────────────────────────────────────────────────
    if (-not (Test-Path -LiteralPath $snap)) {
        if (-not $Update) {
            Write-Host "没有基线快照 $snap；首次运行请加 -Update。"
            exit 2
        }
        Write-Snapshot -Rows $newRows -Names ($orderedNames.ToArray()) -GuardTimeout $gTimeout -GuardStep $gStep -GuardMemory $gMemory
        Update-DocNumbers
        Write-Host ''
        Write-Host "已写入 $snap —— 记得把它和本批改动一起提交。"
        exit 0
    }

    Write-Host ''
    Write-Host "== 与基线 $snap 对比 =="
    $old = @{}
    $oldGuardT = 0; $oldGuardS = 0; $oldGuardM = 0
    $oldHasGuards = $false
    $oldBudget = $null
    foreach ($line in [IO.File]::ReadAllLines((Join-Path $root $snap))) {
        if ($line -match '^#\s*guards:') {
            $oldHasGuards = $true
            if ($line -match 'timeout\s+(\d+).*step-limit\s+(\d+).*memory\s+(\d+)') {
                $oldGuardT = [int]$Matches[1]; $oldGuardS = [int]$Matches[2]; $oldGuardM = [int]$Matches[3]
            }
            if ($line -match 'TEST262_MEMORY_MB=(\d+)') { $oldBudget = [int]$Matches[1] }
            continue
        }
        if ($line -match '^#') { continue }
        $parts = $line -split "`t"
        if ($parts.Count -ge 4 -and $parts[0]) {
            $old[$parts[0]] = [pscustomobject]@{
                Passed = [int]$parts[1]; Skipped = [int]$parts[2]; Failed = [int]$parts[3]
            }
        }
    }

    $regressed = $false
    if ($oldHasGuards) {
        if ($null -ne $oldBudget -and $oldBudget -ne $script:memoryMb) {
            Write-Host ("  护栏预算不同（基线 {0}MB vs 本次 {1}MB）；跳过护栏计数比较。" -f $oldBudget, $script:memoryMb)
        } else {
            $oldTotal = $oldGuardT + $oldGuardS + $oldGuardM
            $newTotal = $gTimeout + $gStep + $gMemory
            if ($newTotal -gt $oldTotal) {
                Write-Host "  护栏计数回退：基线 $oldTotal -> 本次 $newTotal (t/s/m $gTimeout/$gStep/$gMemory)"
                $regressed = $true
            } elseif ($newTotal -lt $oldTotal) {
                Write-Host "  护栏计数改善：$oldTotal -> $newTotal"
            }
        }
    } else {
        Write-Host "  基线未记录护栏计数（旧快照）；本次 t/s/m = $gTimeout/$gStep/$gMemory。"
    }

    $gains = [System.Collections.Generic.List[string]]::new()
    $regressions = [System.Collections.Generic.List[string]]::new()
    $added = [System.Collections.Generic.List[string]]::new()
    $missing = [System.Collections.Generic.List[string]]::new()
    foreach ($name in $orderedNames) {
        if (-not $old.ContainsKey($name)) { $added.Add($name); continue }
        $o = $old[$name]; $n = $newRows[$name]
        if ($n.Passed -lt $o.Passed) {
            $regressions.Add(("  回退  {0,-48} {1} -> {2} （失败 {3} -> {4}）" -f $name, $o.Passed, $n.Passed, $o.Failed, $n.Failed))
            $regressed = $true
        } elseif ($n.Passed -gt $o.Passed) {
            $gains.Add(("  提升  {0,-48} {1} -> {2}" -f $name, $o.Passed, $n.Passed))
        }
    }
    foreach ($name in $old.Keys) {
        if (-not $newRows.ContainsKey($name)) { $missing.Add($name) }
    }

    if ($gains.Count -gt 0) {
        Write-Host ''
        Write-Host '提升的套件：'
        $gains | Select-Object -First 20 | ForEach-Object { Write-Host $_ }
    }
    if ($missing.Count -gt 0) {
        Write-Host ''
        Write-Host '基线里有、本次没有的套件（被移除或改名？）：'
        $missing | ForEach-Object { Write-Host "  $_" }
    }
    if ($added.Count -gt 0) {
        Write-Host ''
        Write-Host '本次新出现的套件：'
        $added | ForEach-Object { Write-Host "  $_" }
    }
    if ($regressions.Count -gt 0) {
        Write-Host ''
        Write-Host '回退的套件（§7.1 要求：解释或回退）：'
        $regressions | ForEach-Object { Write-Host $_ }
    }

    # ── 7. 写回快照 ───────────────────────────────────────────────────────
    if ($Update) {
        Write-Snapshot -Rows $newRows -Names ($orderedNames.ToArray()) -GuardTimeout $gTimeout -GuardStep $gStep -GuardMemory $gMemory
        Update-DocNumbers
        Write-Host ''
        Write-Host "已写入 $snap —— 记得把它和本批改动一起提交（它记录了本批的基线）。"
    }

    Write-Host ''
    if ($regressed) {
        Write-Host '结论：有逐套件回退。'
        $exitCode = 1
    } else {
        Write-Host '结论：无逐套件回退。'
    }
} finally {
    Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

exit $exitCode
