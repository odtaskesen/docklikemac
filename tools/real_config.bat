@echo off
rem Claude kapsayicisinin disinda (explorer.exe ile calistir) gercek ayarlari ve Run anahtarini tools\ altina yazar.
copy /y "%APPDATA%\Dock\config.json" "%~dp0_real_config.json" >nul
reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" > "%~dp0_real_run.txt" 2>&1
