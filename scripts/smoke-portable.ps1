# Smoke test for the portable exe (Windows): launch dist-portable\charon.exe,
# bring it to the front, type into the first field (the Create/Unlock-vault
# master password), and screenshot the window to dist-portable\smoke.png.
# Release builds differ from `tauri dev` in keyboard focus (WebView2 controller
# focus at startup), which is exactly what this exercises. Expect dots in the
# field; an empty field with a focus ring is the bug.
param([string]$Keys = "smoketest", [int]$Wait = 4, [switch]$Keep)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class W {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r);
  [StructLayout(LayoutKind.Sequential)] public struct R { public int L, T, Rt, B; }
}
"@
$root = Split-Path $PSScriptRoot
$exe = Join-Path $root "dist-portable\charon.exe"
if (-not (Test-Path $exe)) { throw "no $exe - run `npm run portable` first" }

$p = Start-Process -FilePath $exe -PassThru
Start-Sleep $Wait
$p.Refresh()
$h = $p.MainWindowHandle
# Foreground-lock workaround: a synthetic Alt press lets SetForegroundWindow succeed.
$ws = New-Object -ComObject WScript.Shell
$ws.SendKeys("%") | Out-Null
[W]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 800
if ([W]::GetForegroundWindow() -ne $h) { Stop-Process -Id $p.Id -Force; throw "could not bring Charon to the foreground; not sending keys" }

[System.Windows.Forms.SendKeys]::SendWait($Keys)
Start-Sleep 1
$r = New-Object W+R
[W]::GetWindowRect($h, [ref]$r) | Out-Null
$b = New-Object System.Drawing.Bitmap ($r.Rt - $r.L), ($r.B - $r.T)
[System.Drawing.Graphics]::FromImage($b).CopyFromScreen($r.L, $r.T, 0, 0, $b.Size)
$shot = Join-Path $root "dist-portable\smoke.png"
$b.Save($shot)
Write-Output "typed '$Keys'; screenshot: $shot"
if (-not $Keep) { Stop-Process -Id $p.Id -Force }
