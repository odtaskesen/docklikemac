# Kullanıcı fareyi dock'a götürdükçe dock bölgesini kaydeder (en fazla 5 olay, 3 dk).
# Her olay için yarım çözünürlükte kare sayfası (rec_N.png) ve parlaklık günlüğü yazar.
param([int]$Events = 5, [int]$TimeoutSec = 180)
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$dir = $PSScriptRoot
$rx = 980; $ry = 1290; $rw = 1480; $rh = 150; $sw = 740; $sh = 75; $n = 90
$deadline = (Get-Date).AddSeconds($TimeoutSec)
$ev = 0
$log = @()
while ($ev -lt $Events -and (Get-Date) -lt $deadline) {
    $p = [System.Windows.Forms.Cursor]::Position
    # Fare dock'un üstündeki bölgeye yaklaşınca kaydı başlat.
    if ($p.Y -ge 1220 -and $p.X -ge $rx -and $p.X -lt ($rx + $rw)) {
        $sheet = New-Object System.Drawing.Bitmap $sw, ($sh * $n)
        $gs = [System.Drawing.Graphics]::FromImage($sheet)
        $frame = New-Object System.Drawing.Bitmap $rw, $rh
        $gf = [System.Drawing.Graphics]::FromImage($frame)
        $t0 = [Environment]::TickCount
        $lines = @()
        for ($i = 0; $i -lt $n; $i++) {
            $gf.CopyFromScreen($rx, $ry, 0, 0, (New-Object System.Drawing.Size $rw, $rh))
            $gs.DrawImage($frame, 0, $i * $sh, $sw, $sh)
            $c = [System.Windows.Forms.Cursor]::Position
            $lines += "{0} t={1}ms imlec=({2},{3})" -f $i, ([Environment]::TickCount - $t0), $c.X, $c.Y
        }
        $sheet.Save("$dir\rec_$ev.png")
        $gs.Dispose(); $gf.Dispose(); $sheet.Dispose(); $frame.Dispose()
        $lines | Set-Content "$dir\rec_$ev.txt"
        $log += "olay $ev kaydedildi"
        $ev++
        # Fare dock bölgesinden çıkana kadar bekle, sonra yeni olayı ara.
        while ([System.Windows.Forms.Cursor]::Position.Y -ge 1150 -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 100 }
    }
    Start-Sleep -Milliseconds 15
}
if ($log.Count -eq 0) { "hiç olay yok" } else { $log }
