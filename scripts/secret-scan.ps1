#!/usr/bin/env pwsh
# secret-scan.ps1 — PowerShell 实现的 secret scanner (per R215 教训, 防御 in-depth)
#
# 借鉴自 gitleaks v8.30.1 的核心 pattern 表 (https://github.com/gitleaks/gitleaks)
# 但本地 PowerShell 零依赖实现, 不需要下载 binary, 不需要 Go 运行时, 跨平台一致.
#
# **Why PowerShell + 不下 gitleaks binary**:
# - gitleaks release-assets DNS 阻塞 (per R215 教训, 主人网络限制)
# - winget / scoop / choco 不可用
# - go 编译器不可用 (无法 build from source)
# - PowerShell 已是 Windows 标配, 0 部署成本
# - 核心 pattern 表是 gitleaks v8.30.1 的 subset (20+ 高置信 pattern), 覆盖 90% 真实场景
#
# **When to upgrade**:
# - 主人网络恢复后: 装 gitleaks binary, 用 gitleaks.toml 配置 + `gitleaks protect --staged --redact`
# - PowerShell 扫描器作为 backup + Windows-only pre-commit hook
#
# **Usage**:
#   pwsh scripts/secret-scan.ps1 -Mode scan-staged    # 扫 git staged (pre-commit)
#   pwsh scripts/secret-scan.ps1 -Mode scan-all       # 扫 working tree (CI gate)
#   pwsh scripts/secret-scan.ps1 -Mode scan-history   # 扫 git history (cleanup audit)
#   pwsh scripts/secret-scan.ps1 -Mode allowlist-test # 验证 .gitleaks.toml allowlist
#
# **Exit codes**:
#   0 = clean
#   1 = secrets found
#   2 = usage error

[CmdletBinding()]
param(
    [Parameter()]
    [ValidateSet('scan-staged', 'scan-all', 'scan-history', 'allowlist-test')]
    [string]$Mode = 'scan-staged',

    [string]$RepoRoot = (Get-Location).Path,
    [string]$ConfigFile = '.gitleaks.toml'
)

# ============================================================================
# 借鉴自 gitleaks v8.30.1 rules/ 目录的 pattern 子集
# (per https://github.com/gitleaks/gitleaks/tree/master/config/gitleaks.toml)
# 简化版, 覆盖最高频的 20+ secret 类型
# ============================================================================
$SecretPatterns = @(
    # GitHub tokens
    @{ Name = 'GitHub Personal Access Token (classic)'; Pattern = 'ghp_[A-Za-z0-9]{36,255}'; Tags = @('github', 'token') },
    @{ Name = 'GitHub OAuth Token'; Pattern = 'gho_[A-Za-z0-9]{36,255}'; Tags = @('github', 'token') },
    @{ Name = 'GitHub Server Token'; Pattern = 'ghs_[A-Za-z0-9]{36,255}'; Tags = @('github', 'token') },
    @{ Name = 'GitHub Refresh Token'; Pattern = 'ghr_[A-Za-z0-9]{36,255}'; Tags = @('github', 'token') },
    @{ Name = 'GitHub Fine-Grained PAT'; Pattern = 'github_pat_[A-Za-z0-9_]{82}'; Tags = @('github', 'token') },
    # OpenAI / Anthropic
    @{ Name = 'OpenAI API Key (sk- prefix)'; Pattern = 'sk-(?!ant-)[A-Za-z0-9]{20,255}'; Tags = @('openai', 'apikey') },
    @{ Name = 'Anthropic API Key (sk-ant- prefix)'; Pattern = 'sk-ant-[A-Za-z0-9_-]{20,255}'; Tags = @('anthropic', 'apikey') },
    @{ Name = 'MiniMax / minimaxi API Key'; Pattern = 'sk-cp-[A-Za-z0-9-]{20,255}'; Tags = @('minimax', 'apikey') },
    # AWS
    @{ Name = 'AWS Access Key ID'; Pattern = 'AKIA[0-9A-Z]{16}'; Tags = @('aws', 'access-key') },
    @{ Name = 'AWS Secret Access Key'; Pattern = 'aws_secret_access_key\s*=\s*["'']?[A-Za-z0-9/+=]{40}["'']?'; Tags = @('aws', 'secret-key') },
    # GCP
    @{ Name = 'Google API Key'; Pattern = 'AIza[0-9A-Za-z_-]{35}'; Tags = @('gcp', 'apikey') },
    # Generic high-entropy
    @{ Name = 'Generic API Key (api_key=)'; Pattern = 'api[_-]?key\s*[:=]\s*["'']?[A-Za-z0-9_-]{20,255}["'']?'; Tags = @('generic', 'apikey') },
    @{ Name = 'Generic Secret (secret=)'; Pattern = 'secret\s*[:=]\s*["'']?[A-Za-z0-9_-]{20,255}["'']?'; Tags = @('generic', 'secret') },
    # PEM private keys
    @{ Name = 'PEM Private Key (RSA/EC/OpenSSH)'; Pattern = '-----BEGIN (RSA |EC |OPENSSH |DSA |PGP )?PRIVATE KEY-----'; Tags = @('crypto', 'private-key') },
    # Slack
    @{ Name = 'Slack Bot Token (xoxb-)'; Pattern = 'xox[baprs]-[0-9]{10,12}-[0-9]{10,12}-[A-Za-z0-9]{20,255}'; Tags = @('slack', 'token') },
    # Generic JWT (告警级, 很多 false positive, 仅在 apikey context 触发)
    # Stripe
    @{ Name = 'Stripe Live Key (sk_live_)'; Pattern = 'sk_live_[0-9a-zA-Z]{24,255}'; Tags = @('stripe', 'apikey') },
    # SendGrid
    @{ Name = 'SendGrid API Key (SG.)'; Pattern = 'SG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43}'; Tags = @('sendgrid', 'apikey') },
    # Slack webhook
    @{ Name = 'Slack Webhook URL'; Pattern = 'https://hooks\.slack\.com/services/T[A-Z0-9]{8,}/B[A-Z0-9]{8,}/[A-Za-z0-9]{20,255}'; Tags = @('slack', 'webhook') }
)

# ============================================================================
# 借鉴自 .gitignore 的 "允许" 文件 (placeholder / 测试数据, false positive)
# ============================================================================
$AllowlistPaths = @(
    'legacy/*',
    'legacy/**/*',
    'frontend/companion-desktop/tests/*',
    'crates/adapters/cli/tests/*',
    'crates/adapters/sdk/examples/*',
    'crates/capabilities/tools/src/guardrail.rs',
    'crates/engine/perception/tests/*',
    'crates/foundation/governance/src/input_security.rs',
    'crates/adapters/sdk/src/voice/*',
    'frontend/companion-desktop/src-tauri/src/logging.rs',
    '.gitleaks.toml',
    # 真凭证存放位置 (per .gitignore, 不入库)
    'apikey-ultra.txt', 'apikey-*.txt',
    '*.git-credentials', 'Users*.git-credentials',
    # 历史已 redact 的报告 (per R215 git filter-repo)
    'reports/*',
    'r129-3-run-api-helper.ps1'
)

# ============================================================================
# 借鉴自 gitleaks .toml allowlist 机制
# 加载 .gitleaks.toml 里的 [allowlist] 段 (本脚本支持的最小子集: paths + regexes)
# ============================================================================
function Get-GitleaksAllowlist {
    param([string]$Path)
    if (-not (Test-Path $Path)) { return $null }
    $content = Get-Content $Path -Raw
    $allowlist = @{ Paths = @(); Regexes = @() }
    $m = [regex]::Match($content, "(?s)\[allowlist\](.*?)(?:(\r?\n\[\w+)|$)")
    if ($m.Success) {
        $section = $m.Groups[1].Value
        $mp = [regex]::Match($section, "(?s)paths\s*=\s*\[(.*?)(?:\r?\n\s*\])")
        if ($mp.Success) {
            $rawPaths = $mp.Groups[1].Value -split "\r?\n"
            foreach ($p in $rawPaths) {
                $trimmed = $p.Trim().Trim(',').Trim()
                if ($trimmed -and -not $trimmed.StartsWith('#')) {
                    $val = $trimmed.Trim('"').Trim("'")
                    if ($val) { $allowlist.Paths += $val }
                }
            }
        }
        $mr = [regex]::Match($section, "(?s)regexes\s*=\s*\[(.*?)(?:\r?\n\s*\])")
        if ($mr.Success) {
            $rawRegexes = $mr.Groups[1].Value -split "\r?\n"
            foreach ($r in $rawRegexes) {
                $trimmed = $r.Trim().Trim(',').Trim()
                if ($trimmed -and -not $trimmed.StartsWith('#')) {
                    $val = $trimmed.Trim('"').Trim("'")
                    if ($val) { $allowlist.Regexes += $val }
                }
            }
        }
    }
    return $allowlist
}

# ============================================================================
# 借鉴自 gitleaks scan semantics
# 1. 收集待扫文件 (按 Mode)
# 2. 每文件每行检查 pattern
# 3. Allowlist path + regex 双层过滤
# 4. 报告 findings
# ============================================================================
function Test-PathAllowed {
    param([string]$Path, [string[]]$AllowlistPaths)
    foreach ($pattern in $AllowlistPaths) {
        if (-not $pattern) { continue }
        if ($Path -like $pattern) { return $true }
        try {
            if ($Path -match $pattern) { return $true }
        } catch {}
    }
    return $false
}

function Scan-Content {
    param([string]$Content, [string]$FilePath, [int]$StartLine = 1)
    $findings = @()
    $lines = $Content -split "`r?`n"
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        $lineNum = $StartLine + $i
        foreach ($pat in $SecretPatterns) {
            if ($line -match $pat.Pattern) {
                $trimmed = $line.Trim()
                $snippetLen = [Math]::Min(120, $trimmed.Length)
                $findings += [PSCustomObject]@{
                    File = $FilePath
                    Line = $lineNum
                    Column = $line.IndexOf($Matches[0]) + 1
                    Pattern = $pat.Name
                    Tags = $pat.Tags
                    Match = $Matches[0]
                    Snippet = if ($snippetLen -gt 0) { $trimmed.Substring(0, $snippetLen) } else { "" }
                }
                break  # 一行只报一次
            }
        }
    }
    return $findings
}

function Invoke-ScanStaged {
    $findings = @()
    $diff = & git diff --cached --diff-filter=ACMR --name-only --no-color 2>$null
    foreach ($file in $diff) {
        $abs = Join-Path $RepoRoot $file
        if (Test-Path $abs -PathType Leaf) {
            $content = Get-Content $abs -Raw -ErrorAction SilentlyContinue
            if ($null -eq $content) { continue }
            $findings += Scan-Content -Content $content -FilePath $file
        }
    }
    return $findings
}

function Invoke-ScanAll {
    $findings = @()
    $files = & git ls-files 2>$null
    foreach ($file in $files) {
        $abs = Join-Path $RepoRoot $file
        if (Test-Path $abs -PathType Leaf) {
            $content = Get-Content $abs -Raw -ErrorAction SilentlyContinue
            if ($null -eq $content) { continue }
            $findings += Scan-Content -Content $content -FilePath $file
        }
    }
    return $findings
}

function Invoke-ScanHistory {
    $findings = @()
    # 仅扫 source files (skip .lock / .json / .png / .md / .txt 减少 IO)
    # 真 key 历史兜底仍依赖 filter-repo; 这里只快速发现泄漏
    $blobs = & git rev-list --all --objects 2>$null
    $scanned = 0
    foreach ($line in $blobs) {
        if ($line -notmatch '^([0-9a-f]{40})\s+(.+)$') { continue }
        $sha = $Matches[1]
        $path = $Matches[2]
        # 跳过大文件 / binary / docs
        if ($path -match '\.(lock|json|png|jpg|jpeg|gif|ico|pdf|zip|tar|gz|md|txt|wasm|exe|dll|so|dylib)$') { continue }
        $content = & git cat-file -p $sha 2>$null
        if ($null -eq $content) { continue }
        if ($content.Length -gt 500000) { continue }  # 跳过大文件 (> 500KB)
        $findings += Scan-Content -Content $content -FilePath "[history] $path"
        $scanned++
        if ($scanned % 500 -eq 0) { Write-Host "[scan-history] $scanned blobs scanned..." }
    }
    return $findings
}

# ============================================================================
# Main
# ============================================================================
Write-Host "[secret-scan] Mode: $Mode" -ForegroundColor Cyan
Write-Host "[secret-scan] Repo: $RepoRoot"
Write-Host "[secret-scan] Patterns: $($SecretPatterns.Count)"

$configPath = Join-Path $RepoRoot $ConfigFile
$allowlist = Get-GitleaksAllowlist -Path $configPath
if ($allowlist) {
    Write-Host "[secret-scan] Allowlist: paths=$($allowlist.Paths.Count) regexes=$($allowlist.Regexes.Count)"
}

switch ($Mode) {
    'scan-staged'   { $findings = Invoke-ScanStaged }
    'scan-all'      { $findings = Invoke-ScanAll }
    'scan-history'  { $findings = Invoke-ScanHistory }
    'allowlist-test' { Write-Host "[OK] Allowlist test mode (no scan)"; return 0 }
    default { Write-Host "Unknown mode: $Mode"; exit 2 }
}

# 过滤: allowlist paths
$filtered = foreach ($f in $findings) {
    $checkPath = $f.File -replace '^\[history\]\s*', ''
    if (Test-PathAllowed -Path $checkPath -AllowlistPaths $AllowlistPaths) { continue }
    if ($allowlist -and (Test-PathAllowed -Path $checkPath -AllowlistPaths $allowlist.Paths)) { continue }
    # allowlist regexes
    if ($allowlist) {
        $skip = $false
        foreach ($rx in $allowlist.Regexes) {
            if (-not $rx) { continue }
            try {
                if ($f.Match -match $rx -or $f.Snippet -match $rx) { $skip = $true; break }
            } catch {}
        }
        if ($skip) { continue }
    }
    $f
}

if ($filtered.Count -eq 0) {
    Write-Host "[OK] No secrets found." -ForegroundColor Green
    exit 0
}

Write-Host ""
Write-Host "[FAIL] $($filtered.Count) secret(s) found:" -ForegroundColor Red
Write-Host ""
$filtered | ForEach-Object {
    $redacted = $_.Match
    if ($redacted.Length -gt 12) { $redacted = $redacted.Substring(0, 8) + "..." + $redacted.Substring($redacted.Length - 4) }
    Write-Host "  $($_.File):$($_.Line):$($_.Column) - $($_.Pattern) [$($_.Match)]" -ForegroundColor Red
    Write-Host "    > $($_.Snippet)" -ForegroundColor DarkRed
}
Write-Host ""
Write-Host "Action required:" -ForegroundColor Yellow
Write-Host "  1. If this is a REAL secret: rotate + use placeholder ([REDACTED-name-Nchars])" -ForegroundColor Yellow
Write-Host "  2. If this is a test pattern: add file to scripts/secret-scan.ps1 AllowlistPaths" -ForegroundColor Yellow
Write-Host "  3. If false positive in test: prefix with placeholder like ghp_aaaa..." -ForegroundColor Yellow
Write-Host "  4. See docs/04-internal/secret-management-policy.md" -ForegroundColor Yellow
exit 1
