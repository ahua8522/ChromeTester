# 绘制Chrome风格版本管理器图标并打包多尺寸ICO
Add-Type -AssemblyName System.Drawing

function Draw-Icon([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    $s = $size / 1024.0   # 缩放系数（以1024为基准设计）

    # 配色
    $light  = [System.Drawing.Color]::FromArgb(138, 180, 248)  # 浅蓝
    $medium = [System.Drawing.Color]::FromArgb(66, 133, 244)   # 中蓝
    $deep   = [System.Drawing.Color]::FromArgb(26, 86, 196)    # 深蓝
    $gray   = [System.Drawing.Color]::FromArgb(110, 130, 160)  # 蓝灰（背景层叠圈）
    $white  = [System.Drawing.Color]::White

    # 主轮中心与半径（偏右下，给左上层叠圈留空间）
    $cx = 580 * $s; $cy = 580 * $s; $r = 380 * $s

    # 背景两个层叠外圈（示意多版本），只画轮廓
    if ($size -ge 48) {
        $penW = [Math]::Max(1.0, 16 * $s)
        $pen = New-Object System.Drawing.Pen $gray, $penW
        $g.DrawEllipse($pen, [float](($cx - 150 * $s) - $r), [float](($cy - 150 * $s) - $r), [float](2 * $r), [float](2 * $r))
        $pen2 = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(150, $gray)), $penW
        $g.DrawEllipse($pen2, [float](($cx - 280 * $s) - $r), [float](($cy - 280 * $s) - $r), [float](2 * $r), [float](2 * $r))
        $pen.Dispose(); $pen2.Dispose()
    }

    # 主轮：三段120°扇形（Chrome式Y形分割，边界在-90°/30°/150°）
    $rect = New-Object System.Drawing.RectangleF ([float]($cx - $r)), ([float]($cy - $r)), ([float](2 * $r)), ([float](2 * $r))
    $b1 = New-Object System.Drawing.SolidBrush $light
    $b2 = New-Object System.Drawing.SolidBrush $medium
    $b3 = New-Object System.Drawing.SolidBrush $deep
    $g.FillPie($b1, $rect.X, $rect.Y, $rect.Width, $rect.Height, -90, 120)
    $g.FillPie($b2, $rect.X, $rect.Y, $rect.Width, $rect.Height, 30, 120)
    $g.FillPie($b3, $rect.X, $rect.Y, $rect.Width, $rect.Height, 150, 120)
    $b1.Dispose(); $b2.Dispose(); $b3.Dispose()

    # 中心：白色圆盘 + 中蓝内环
    $rw = 185 * $s
    $bw = New-Object System.Drawing.SolidBrush $white
    $g.FillEllipse($bw, [float]($cx - $rw), [float]($cy - $rw), [float](2 * $rw), [float](2 * $rw))
    $bw.Dispose()
    $ringW = [Math]::Max(1.0, 30 * $s)
    $rr = 135 * $s
    $penR = New-Object System.Drawing.Pen $medium, $ringW
    $g.DrawEllipse($penR, [float]($cx - $rr), [float]($cy - $rr), [float](2 * $rr), [float](2 * $rr))
    $penR.Dispose()

    $g.Dispose()
    return $bmp
}

# 生成各尺寸PNG字节
$sizes = 256, 128, 64, 48, 32, 16
$pngs = @{}
foreach ($sz in $sizes) {
    $bmp = Draw-Icon $sz
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngs[$sz] = $ms.ToArray()
    $ms.Dispose(); $bmp.Dispose()
}

# 另存1024预览图
$preview = Draw-Icon 1024
$preview.Save("e:\Documents\chrome-version-manager\icon-preview.png", [System.Drawing.Imaging.ImageFormat]::Png)
$preview.Dispose()

# 打包ICO（PNG压缩条目，Vista+支持）
$icoPath = "e:\Documents\chrome-version-manager\src-tauri\icons\icon.ico"
$fs = [System.IO.File]::Create($icoPath)
$bw2 = New-Object System.IO.BinaryWriter $fs
# ICONDIR
$bw2.Write([uint16]0); $bw2.Write([uint16]1); $bw2.Write([uint16]$sizes.Count)
# 目录条目
$offset = 6 + 16 * $sizes.Count
foreach ($sz in $sizes) {
    $dim = if ($sz -ge 256) { 0 } else { $sz }
    $bw2.Write([byte]$dim)          # 宽
    $bw2.Write([byte]$dim)          # 高
    $bw2.Write([byte]0)             # 调色板
    $bw2.Write([byte]0)             # 保留
    $bw2.Write([uint16]1)           # 平面
    $bw2.Write([uint16]32)          # 位深
    $bw2.Write([uint32]$pngs[$sz].Length)
    $bw2.Write([uint32]$offset)
    $offset += $pngs[$sz].Length
}
# 图像数据
foreach ($sz in $sizes) { $bw2.Write($pngs[$sz]) }
$bw2.Close(); $fs.Close()

Get-Item $icoPath | Select-Object Name, Length
Write-Output "完成：含 $($sizes -join '/') 六种尺寸"
