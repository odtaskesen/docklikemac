# İmleci dock'a hızlıca sokar (birkaç büyük adımla), girişten sonraki karelerde
# büyüme boşluğunun parlaklığını ölçer. Flaş (> 100) sayısını yazar.
param([int]$Trials = 15)
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @"
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
public static class F {
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    public static double Bright(Bitmap b) {
        var d = b.LockBits(new Rectangle(0, 0, b.Width, b.Height), ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
        int n = d.Stride * b.Height; var buf = new byte[n]; Marshal.Copy(d.Scan0, buf, 0, n); b.UnlockBits(d);
        double s = 0; int c = 0;
        for (int i = 0; i < n; i += 4) { s += buf[i] + buf[i+1] + buf[i+2]; c++; }
        return s / (3.0 * c);
    }
}
"@ -ReferencedAssemblies System.Drawing.Common, System.Drawing.Primitives, System.Runtime.InteropServices
$orig = [System.Windows.Forms.Cursor]::Position
$bx = 1000; $by = 1346; $bw = 1400; $bh = 22
$b = New-Object System.Drawing.Bitmap $bw, $bh
$g = [System.Drawing.Graphics]::FromImage($b)
$flashes = 0; $lines = @()
for ($t = 0; $t -lt $Trials; $t++) {
    $x = 1250 + ($t % 8) * 60
    [F]::SetCursorPos($x, 1100) | Out-Null
    Start-Sleep -Milliseconds 600
    # Hızlı hareket: 4 büyük adım.
    foreach ($y in 1200, 1300, 1360, 1400) { [F]::SetCursorPos($x + 5, $y) | Out-Null; Start-Sleep -Milliseconds 6 }
    $max = 0
    for ($k = 0; $k -lt 40; $k++) {
        $g.CopyFromScreen($bx, $by, 0, 0, (New-Object System.Drawing.Size $bw, $bh))
        $v = [F]::Bright($b); if ($v -gt $max) { $max = $v }
    }
    if ($max -gt 100) { $flashes++ }
    $lines += ("{0:N0}" -f $max)
}
[F]::SetCursorPos($orig.X, $orig.Y) | Out-Null
$g.Dispose(); $b.Dispose()
"flaş: $flashes / $Trials   (en yüksek parlaklıklar: " + ($lines -join ", ") + ")"
