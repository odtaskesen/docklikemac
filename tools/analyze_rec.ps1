param([int]$Ev = 0)
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
public static class A {
    public static double[] Rows(Bitmap b, int fh, int frame, int y0, int y1) {
        var d = b.LockBits(new Rectangle(0, frame * fh + y0, b.Width, y1 - y0), ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
        int n = d.Stride * (y1 - y0); var buf = new byte[n]; Marshal.Copy(d.Scan0, buf, 0, n); b.UnlockBits(d);
        double s = 0; double mx = 0; int c = 0;
        for (int i = 0; i < n; i += 4) { double v = (buf[i] + buf[i+1] + buf[i+2]) / 3.0; s += v; if (v > mx) mx = v; c++; }
        return new double[] { s / c, mx };
    }
}
"@ -ReferencedAssemblies System.Drawing.Common, System.Drawing.Primitives, System.Runtime.InteropServices
$dir = $PSScriptRoot
$b = [System.Drawing.Bitmap]::FromFile("$dir\rec_$Ev.png")
$fh = 75; $n = [int]($b.Height / $fh)
$cur = Get-Content "$dir\rec_$Ev.txt"
for ($i = 0; $i -lt $n; $i++) {
    $top = [A]::Rows($b, $fh, $i, 0, 24)     # pencerenin üstü (dışarısı)
    $head = [A]::Rows($b, $fh, $i, 26, 41)   # büyüme boşluğu
    $body = [A]::Rows($b, $fh, $i, 42, 73)   # gövde
    "{0}  dis={1,6:N1}  bosluk={2,6:N1} (max {3,5:N0})  govde={4,6:N1}   {5}" -f $i, $top[0], $head[0], $head[1], $body[0], ($cur[$i] -replace '^\d+ ','')
}
$b.Dispose()
