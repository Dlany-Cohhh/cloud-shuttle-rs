#Requires -Version 5.1
<#
  云梭 CloudShuttle · Rust 原生版 Windows 构建脚本
  用法：
    powershell -ExecutionPolicy Bypass -File scripts\build.ps1
    powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -StaticDir E:\A-MyProject\cloud-shuttle\static -OutDir dist\windows
    powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -DebugBuild
#>
[CmdletBinding()]
param(
    [string]$StaticDir = "",
    [string]$OutDir = "",
    [switch]$DebugBuild,
    [switch]$NoStatic
)

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

if (-not $OutDir) { $OutDir = Join-Path $Root 'dist\windows' }
if (-not $StaticDir) {
    $candidates = @(
        (Join-Path $Root 'static'),
        (Join-Path (Split-Path -Parent $Root) 'cloud-shuttle\static')
    )
    foreach ($c in $candidates) { if (Test-Path (Join-Path $c 'index.html')) { $StaticDir = $c; break } }
}

Write-Host "==> cargo build ($(if ($DebugBuild) {'debug'} else {'release'}))"
if ($DebugBuild) { & cargo build } else { & cargo build --release }
if ($LASTEXITCODE -ne 0) { throw "cargo build 失败，退出码 $LASTEXITCODE" }

$built = Join-Path $Root "target\$(if ($DebugBuild) {'debug'} else {'release'})\cloud-shuttle-rs.exe"
if (-not (Test-Path $built)) { throw "找不到编译产物：$built" }

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
Copy-Item -Force $built (Join-Path $OutDir 'cloud-shuttle-rs.exe')

if (-not $NoStatic) {
    if (-not $StaticDir -or -not (Test-Path (Join-Path $StaticDir 'index.html'))) {
        Write-Warning "未找到前端 static 目录，可用 -StaticDir 指定；运行时也可用 --static-dir 指定。"
    } else {
        $target = Join-Path $OutDir 'static'
        New-Item -ItemType Directory -Force -Path $target | Out-Null
        Copy-Item -Force -Recurse (Join-Path $StaticDir '*') $target
        Write-Host "==> 前端资源已复制：$target"
    }
}

Copy-Item -Force (Join-Path $Root '.env.example') (Join-Path $OutDir '.env.example') -ErrorAction SilentlyContinue
Write-Host ""
Write-Host "构建完成：$OutDir"
Write-Host "  自检：.\dist\windows\cloud-shuttle-rs.exe --check"
Write-Host "  启动：.\dist\windows\cloud-shuttle-rs.exe"
