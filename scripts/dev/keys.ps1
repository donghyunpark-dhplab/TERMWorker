param([string[]]$Keys, [int]$DelayMs = 250, [string]$Proc = "termworker")
# Send keystrokes to the app window (SendKeys syntax: ^ = Ctrl, + = Shift, {ENTER}).
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class F { [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h); }
"@
$p = Get-Process $Proc -ErrorAction Stop | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
[F]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
Start-Sleep -Milliseconds 300
foreach ($k in $Keys) {
  [System.Windows.Forms.SendKeys]::SendWait($k)
  Start-Sleep -Milliseconds $DelayMs
}
