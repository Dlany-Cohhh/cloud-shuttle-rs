#Requires -Version 5.1
<#
  把 Rust 原生控制台注册为「登录时自动启动」的计划任务（无需管理员权限）。
  用法：
    powershell -ExecutionPolicy Bypass -File packaging\install-service.ps1 -ExePath C:\Apps\cloud-shuttle-rs\cloud-shuttle-rs.exe
    powershell -ExecutionPolicy Bypass -File packaging\install-service.ps1 -Uninstall
#>
[CmdletBinding()]
param(
    [string]$ExePath = "",
    [string]$WorkDir = "",
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'
$TaskName = 'CloudShuttle Rust Console'

if ($Uninstall) {
    if (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) {
        Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
        Write-Host "已移除计划任务：$TaskName"
    } else {
        Write-Host "计划任务不存在，无需移除。"
    }
    return
}

if (-not $ExePath) { $ExePath = Join-Path $PSScriptRoot '..\dist\windows\cloud-shuttle-rs.exe' }
$ExePath = [System.IO.Path]::GetFullPath($ExePath)
if (-not (Test-Path $ExePath)) { throw "找不到程序：$ExePath" }
if (-not $WorkDir) { $WorkDir = Split-Path -Parent $ExePath }

$action = New-ScheduledTaskAction -Execute $ExePath -WorkingDirectory $WorkDir
$trigger = New-ScheduledTaskTrigger -AtLogOn -User "$env:USERDOMAIN\$env:USERNAME"
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero)

if (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) {
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
}
Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger -Settings $settings `
    -Description '云梭 CloudShuttle Rust 原生下载控制台' | Out-Null
Write-Host "已注册登录自启：$TaskName"
Write-Host "  程序：$ExePath"
Write-Host "  立即启动：Start-ScheduledTask -TaskName '$TaskName'"
Write-Host "  停止：Stop-ScheduledTask -TaskName '$TaskName'"
