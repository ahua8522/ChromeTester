# 生成内置快照：Chrome 版本列表 + Chromium 里程碑
$snapDir = "e:\Documents\chrome-version-manager\src-tauri\snapshot"
New-Item -ItemType Directory -Force -Path $snapDir | Out-Null

# 1. Chrome for Testing 版本列表（从 npmmirror 目录列表取，免代理）
Write-Output "抓取 Chrome 版本列表..."
$cft = Invoke-RestMethod -Uri 'https://registry.npmmirror.com/-/binary/chrome-for-testing/' -NoProxy -TimeoutSec 30
$versions = $cft | Where-Object { $_.type -eq 'dir' } | ForEach-Object { $_.name.TrimEnd('/') } |
    Where-Object { $_ -match '^\d+(\.\d+)+$' }
# 按版本号倒序
$versions = $versions | Sort-Object -Property @{Expression={[version]($_ -replace '^(\d+\.\d+\.\d+).*','$1.0')}} -Descending -ErrorAction SilentlyContinue
# 简单倒序（字符串数字分段）作为兜底
$versions = $cft | Where-Object { $_.type -eq 'dir' } | ForEach-Object { $_.name.TrimEnd('/') } | Where-Object { $_ -match '^\d+(\.\d+)+$' }
$sorted = $versions | Sort-Object { ,@($_.Split('.') | ForEach-Object {[int]$_}) }
[array]::Reverse($sorted)
$sorted | ConvertTo-Json -Compress | Set-Content "$snapDir\chrome-versions.json" -Encoding UTF8
Write-Output "Chrome 版本数: $($sorted.Count)  最新: $($sorted[0])  最老: $($sorted[-1])"

# 2. Chromium 里程碑（chromiumdash，走系统代理）
Write-Output "抓取 Chromium 里程碑..."
$ms = Invoke-RestMethod -Uri 'https://chromiumdash.appspot.com/fetch_milestones?only_branched=true' -TimeoutSec 30
$pairs = $ms | Where-Object { [int]$_.milestone -lt 113 -and [int]$_.chromium_main_branch_position -gt 0 } |
    ForEach-Object { @([int]$_.milestone, [int64]$_.chromium_main_branch_position) }
# 转成 [[milestone,position],...]
$arr = @()
foreach ($m in ($ms | Where-Object { [int]$_.milestone -lt 113 -and [int]$_.chromium_main_branch_position -gt 0 })) {
    $arr += ,@([int]$m.milestone, [int64]$m.chromium_main_branch_position)
}
$arr | ConvertTo-Json -Compress -Depth 3 | Set-Content "$snapDir\chromium-milestones.json" -Encoding UTF8
Write-Output "里程碑数: $($arr.Count)  范围: M$(($arr | ForEach-Object {$_[0]} | Measure-Object -Minimum).Minimum) ~ M$(($arr | ForEach-Object {$_[0]} | Measure-Object -Maximum).Maximum)"
Write-Output "快照文件:"; Get-ChildItem $snapDir | Select-Object Name, Length
