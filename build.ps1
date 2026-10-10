<#
.SYNOPSIS
DSH Manager 一键打包脚本（Tauri 2 + React）

.DESCRIPTION
1. （可选）版本号升级：-Version 0.3.16 → 自动同步 package.json / tauri.conf.json / VERSION /
   Cargo.toml / Cargo.lock / App.tsx / App.test.tsx / lib.rs 中的版本号
2. 自动关闭正在运行的 dsh-manager 进程（防止 exe 被占用导致打包失败）
3. tsc 类型检查
4. cargo test（后端）+ npm test（前端），可用 -SkipTests 跳过
5. npx tauri build 打包
6. 输出产物路径与大小

.EXAMPLE
.\build.ps1                    # 直接重新打包当前版本
.\build.ps1 -Version 0.3.16    # 升版到 0.3.16 并打包
.\build.ps1 -SkipTests         # 跳过测试，只做类型检查 + 打包
#>
param(
    [string]$Version = "",
    [switch]$SkipTests
)
$ErrorActionPreference = "Continue"
$root = $PSScriptRoot
Set-Location $root

$utf8NoBom = New-Object System.Text.UTF8Encoding $false

function Write-Step([string]$msg) { Write-Host "`n==== $msg ====" -ForegroundColor Cyan }
function Fail([string]$msg) { Write-Host "[FAIL] $msg" -ForegroundColor Red; exit 1 }

# ---------- 0) 当前版本 ----------
Write-Step "读取当前版本"
$pkg = Get-Content "$root\package.json" -Raw -Encoding UTF8 | ConvertFrom-Json
$oldVer = [string]$pkg.version
Write-Host "当前版本: $oldVer"

# ---------- 1) 版本升级 ----------
if ($Version) {
    if ($oldVer -eq $Version) {
        Write-Host "已是 $Version，跳过版本更新" -ForegroundColor Yellow
    } else {
        Write-Step "版本升级 $oldVer -> $Version"
        foreach ($f in @("$root\package.json", "$root\src-tauri\tauri.conf.json", "$root\VERSION")) {
            $t = [IO.File]::ReadAllText($f)
            if (-not $t.Contains($oldVer)) { Fail "$f 中未找到 $oldVer" }
            [IO.File]::WriteAllText($f, $t.Replace($oldVer, $Version), $utf8NoBom)
        }
        # Cargo.toml / Cargo.lock：只改 dsh-manager 自身块
        foreach ($f in @("$root\src-tauri\Cargo.toml", "$root\src-tauri\Cargo.lock")) {
            $t = [IO.File]::ReadAllText($f)
            $pat = 'name = "dsh-manager"`r`nversion = "[^"]+"'
            $repl = 'name = "dsh-manager"`r`nversion = "' + $Version + '"'
            $n = [regex]::Replace($t, $pat, $repl)
            if ($n -eq $t) { Fail "$f 中未找到 dsh-manager 版本块" }
            [IO.File]::WriteAllText($f, $n, $utf8NoBom)
        }
        # 源码内版本引用（可能没有，未找到不算错）
        foreach ($f in @("$root\src\App.tsx", "$root\src\App.test.tsx", "$root\src-tauri\src\lib.rs")) {
            $t = [IO.File]::ReadAllText($f)
            if ($t.Contains($oldVer)) {
                [IO.File]::WriteAllText($f, $t.Replace($oldVer, $Version), $utf8NoBom)
            }
        }
        Write-Host "版本文件已更新" -ForegroundColor Green
    }
}

# ---------- 2) 关闭占用进程 ----------
Write-Step "关闭 dsh-manager 进程（防 exe 占用）"
$proc = Get-Process dsh-manager -ErrorAction SilentlyContinue
if ($proc) {
    Write-Host "关闭 PID: $($proc.Id -join ', ')"
    Stop-Process -Name dsh-manager -Force -Confirm:$false -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
} else {
    Write-Host "无运行中的 dsh-manager"
}

# ---------- 3) tsc 类型检查 ----------
Write-Step "[1/4] tsc 类型检查"
npx tsc --noEmit
if ($LASTEXITCODE -ne 0) { Fail "tsc 类型检查失败" }
Write-Host "tsc 通过" -ForegroundColor Green

# ---------- 4) 测试 ----------
if ($SkipTests) {
    Write-Step "[2/4] 已跳过测试（-SkipTests）"
} else {
    Write-Step "[2/4] 后端测试 cargo test"
    try { $out = cargo test --manifest-path "$root\src-tauri\Cargo.toml" --lib 2>&1 | Out-String } catch { $out = [string]$_ }
    $out | Select-String -Pattern "test result: FAILED|error\[|^error:" | ForEach-Object { Write-Host $_ }
    if ($LASTEXITCODE -ne 0 -and ($out -match "test result: FAILED|error\[E|^error:")) {
        Fail "后端测试失败"
    }
    $out | Select-String -Pattern "test result:" | Select-Object -Last 1 | ForEach-Object { Write-Host $_ }

    Write-Step "[3/4] 前端测试 npm test"
    npm test
    if ($LASTEXITCODE -ne 0) { Fail "前端测试失败" }
}

# ---------- 5) 打包 ----------
Write-Step "[4/4] npx tauri build"
npx tauri build
if ($LASTEXITCODE -ne 0) { Fail "tauri build 失败" }

# ---------- 6) 产物 ----------
Write-Step "产物"
$exe = "$root\src-tauri\target\release\dsh-manager.exe"
$setup = Get-ChildItem "$root\src-tauri\target\release\bundle\nsis\DSH Manager_*_x64-setup.exe" |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (Test-Path $exe) {
    $i = Get-Item $exe
    Write-Host ("单文件 exe : {0}  ({1:N2} MB)" -f $i.FullName, ($i.Length / 1MB))
}
if ($setup) {
    Write-Host ("安装包     : {0}  ({1:N2} MB)" -f $setup.FullName, ($setup.Length / 1MB))
}
Write-Host "`n打包完成 ✔" -ForegroundColor Green
exit 0
