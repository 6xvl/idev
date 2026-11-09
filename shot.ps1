# Screenshot the idev window even when covered (PrintWindow + PW_RENDERFULLCONTENT), no focus change. Usage: shot.ps1 [pid]
param([int]$procId = (Get-Process idev | Select-Object -First 1).Id)
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public class IdevShot { [StructLayout(LayoutKind.Sequential)] public struct R { public int L,T,Rt,B; }
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r);
[DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f); }
'@
$h = (Get-Process -Id $procId).MainWindowHandle
$r = New-Object IdevShot+R
[IdevShot]::GetWindowRect($h, [ref]$r) | Out-Null
$bmp = New-Object System.Drawing.Bitmap ($r.Rt - $r.L), ($r.B - $r.T)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$dc = $g.GetHdc()
[IdevShot]::PrintWindow($h, $dc, 2) | Out-Null
$g.ReleaseHdc($dc)
$bmp.Save("$PSScriptRoot\shot.png")
