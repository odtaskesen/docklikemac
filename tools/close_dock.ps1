# Dock'u düzgün kapatır: dock sürecinin Slint penceresine WM_CLOSE gönderir
# (Slint olay döngüsünden çıkar, cleanup() görev çubuğunu geri getirir).
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class W {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
}
"@
$p = Get-Process dock -ErrorAction SilentlyContinue
if (!$p) { "dock çalışmıyor"; exit }
$target = [IntPtr]::Zero
[W]::EnumWindows({ param($h, $l)
    $pid2 = 0; [W]::GetWindowThreadProcessId($h, [ref]$pid2) | Out-Null
    if ($pid2 -eq $p.Id) {
        $t = New-Object System.Text.StringBuilder 64; [W]::GetWindowTextW($h, $t, 64) | Out-Null
        if ($t.ToString() -eq "Dock") { $script:target = $h; return $false }
    }
    return $true }, [IntPtr]::Zero) | Out-Null
if ($target -eq [IntPtr]::Zero) { "dock penceresi bulunamadı"; exit }
[W]::PostMessageW($target, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
$t = 0
while ((Get-Process dock -ErrorAction SilentlyContinue) -and $t -lt 50) { Start-Sleep -Milliseconds 100; $t++ }
if (Get-Process dock -ErrorAction SilentlyContinue) { "kapanmadı" } else { "kapandı" }
