param([int]$X = 1400, [int]$StartY = 1280, [int]$EndY = 1405, [int]$Frames = 24, [switch]$Warm, [string]$Tag = "x")
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @"
using System.Runtime.InteropServices;
public static class Cur { [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y); }
"@
$orig = [System.Windows.Forms.Cursor]::Position
$rx = 1000; $ry = 1310; $rw = 1450; $rh = 130
if ($Warm) {
    # Önce dock'u büyüt, sonra çık (gerçek kullanımda olduğu gibi).
    [Cur]::SetCursorPos($X, $EndY) | Out-Null
    Start-Sleep -Milliseconds 900
    [Cur]::SetCursorPos($X, 1150) | Out-Null
    Start-Sleep -Milliseconds 1200
}
[Cur]::SetCursorPos($X, $StartY) | Out-Null
Start-Sleep -Milliseconds 400
$sheet = New-Object System.Drawing.Bitmap $rw, ($rh * $Frames)
$g = [System.Drawing.Graphics]::FromImage($sheet)
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$times = @()
for ($i = 0; $i -lt $Frames; $i++) {
    if ($i -eq 2) { [Cur]::SetCursorPos($X, $EndY) | Out-Null; "fare girdi: " + ([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() % 100000) }
    $g.CopyFromScreen($rx, $ry, 0, $i * $rh, (New-Object System.Drawing.Size $rw, $rh))
    $times += ([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() % 100000)
}
[Cur]::SetCursorPos($orig.X, $orig.Y) | Out-Null
$sheet.Save("$PSScriptRoot\frames_" + $Tag + ".png")
"kare zamanları (ms): " + ($times -join ", ")

