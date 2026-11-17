<#
.SYNOPSIS
    构建并检查 Windows x64 便携单 EXE。作者：ahui。
.DESCRIPTION
    优先使用仓库 .tools 下的隔离 Rust/MSVC 工具链，否则使用当前系统开发环境。
    不修改全局 PATH；依赖按 package-lock.json/Cargo.lock 固定。需 Node.js、Rust MSVC 和 Windows SDK。
    默认执行前端检查、Rust测试与发布构建；SkipTests 仅用于已完成同一版本测试后的重复打包。
.PARAMETER OutputDirectory
    相对项目根目录的输出目录，默认 dist。旧版 EXE 正在运行时可指定其它目录，避免覆盖占用中的文件。
#>
param(
    [switch]$SkipTests,
    [string]$OutputDirectory = 'dist'
)
$ErrorActionPreference = 'Stop'
$OutputEncoding = [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Set-Location -LiteralPath $projectRoot

# 导入本地MSVC设置，不启动可见终端，也不写入用户环境变量。
$compilerSetup = Join-Path $projectRoot '.tools\msvc\setup_x64.bat'
if (Test-Path -LiteralPath $compilerSetup) {
    $variables = & cmd.exe /d /c "call `"$compilerSetup`" >nul && set"
    foreach ($line in $variables) {
        if ($line -match '^([^=]+)=(.*)$') { [Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'Process') }
    }
}
$localCargo = Join-Path $projectRoot '.tools\cargo'
if (Test-Path -LiteralPath (Join-Path $localCargo 'bin\cargo.exe')) {
    $env:CARGO_HOME = $localCargo
    $env:RUSTUP_HOME = Join-Path $projectRoot '.tools\rustup'
    $env:PATH = (Join-Path $localCargo 'bin') + ';' + $env:PATH
}

function Invoke-Checked {
    param([string]$Program, [string[]]$Arguments)
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "构建步骤失败：$Program（退出码 $LASTEXITCODE）" }
}

if (-not (Test-Path -LiteralPath 'node_modules')) { Invoke-Checked 'npm.cmd' @('ci') }
# 元数据与声明属于发布边界，即使重复打包跳过回归也必须核对。
Invoke-Checked 'node.exe' @('scripts/check-release.mjs')
Invoke-Checked 'node.exe' @('scripts/generate-notices.mjs', '--check')
if (-not $SkipTests) {
    Invoke-Checked 'npm.cmd' @('run', 'format:check')
    Invoke-Checked 'npm.cmd' @('run', 'typecheck')
    # 统一排序属于展示层业务规则，默认构建需覆盖数字精度、Unicode 和稳定 ID。
    Invoke-Checked 'npm.cmd' @('run', 'test:sort')
    Invoke-Checked 'npm.cmd' @('run', 'test:preferences')
    Invoke-Checked 'npm.cmd' @('run', 'test:groups')
    Invoke-Checked 'npm.cmd' @('run', 'test:ui')
    Invoke-Checked 'cargo.exe' @('fmt', '--all', '--', '--check')
    Invoke-Checked 'cargo.exe' @('test', '--workspace', '--locked')
    Invoke-Checked 'cargo.exe' @('clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings')
}
Invoke-Checked 'npm.cmd' @('run', 'tauri', '--', 'build', '--no-bundle')
# 仅切换交付位置，保持 EXE 与 SHA256 在同一目录；不关闭或替换正在运行的旧版。
$releaseDirectory = [System.IO.Path]::GetFullPath((Join-Path $projectRoot $OutputDirectory))
New-Item -ItemType Directory -Path $releaseDirectory -Force | Out-Null
$destination = Join-Path $releaseDirectory 'batch-send-email.exe'
Copy-Item -LiteralPath (Join-Path $projectRoot 'target\release\batch-send-email.exe') -Destination $destination -Force
$hash = (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash
[System.IO.File]::WriteAllText((Join-Path $releaseDirectory 'SHA256.txt'), "$hash  batch-send-email.exe`r`n", [System.Text.UTF8Encoding]::new($false))
Write-Output "构建完成：$destination"
