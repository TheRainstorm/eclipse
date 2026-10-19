param(
    [string]$Output = "$env:TEMP\uu-rust-keyboard-proof.json",
    [ValidateRange(5, 180)][int]$Seconds = 45
)
# Run interactively on a dedicated remote test desktop. Only this form's text
# is inspected. Ctrl+C/V/X are counted and suppressed; system clipboard stays intact.
Add-Type -AssemblyName System.Windows.Forms
Add-Type -ReferencedAssemblies System.Windows.Forms -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public class UUInputForm : System.Windows.Forms.Form {
 public System.Windows.Forms.TextBox Input;
 protected override void OnHandleCreated(EventArgs e) {base.OnHandleCreated(e); UUFocus.RegisterHotKey(Handle, 10723, 0x4000, 0x85); UUFocus.RegisterHotKey(Handle,10724,0x4008,0x86);}
 protected override void WndProc(ref System.Windows.Forms.Message m) {
  if(m.Msg==0x312 && m.WParam.ToInt32()==10723) {UUFocus.SetForegroundWindow(Handle);Activate();if(Input!=null)Input.Focus();}
  base.WndProc(ref m);
 }
 protected override void OnFormClosed(System.Windows.Forms.FormClosedEventArgs e) {UUFocus.UnregisterHotKey(Handle,10723);UUFocus.UnregisterHotKey(Handle,10724);base.OnFormClosed(e);}
}
public static class UUFocus { [DllImport("user32.dll")] public static extern bool RegisterHotKey(IntPtr window,int id,uint mods,uint key); [DllImport("user32.dll")] public static extern bool UnregisterHotKey(IntPtr window,int id); [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow(); [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd); }
'@
$previous=[UUFocus]::GetForegroundWindow()
$form=New-Object UUInputForm
$form.Text='UU Rust Linux input validation'; $form.Width=800; $form.Height=300
$form.StartPosition='Manual'; $form.Left=100; $form.Top=100; $form.TopMost=$true
$text=New-Object Windows.Forms.TextBox; $text.ImeMode=[Windows.Forms.ImeMode]::Disable; $text.Multiline=$true; $text.Dock='Bottom'; $text.Height=90; $text.Font=New-Object Drawing.Font('Consolas',24)
$form.Controls.Add($text); $form.Input=$text
$panel=New-Object Windows.Forms.Panel; $panel.Dock='Fill'; $form.Controls.Add($panel)
$panel.BringToFront()
$script:tick=0
$panel.add_Paint({param($sender,$event)
 $event.Graphics.Clear([Drawing.Color]::FromArgb(30,35,45))
 $x=($script:tick*17)%([Math]::Max(1,$sender.Width-120))
 $event.Graphics.FillRectangle([Drawing.Brushes]::DeepSkyBlue,$x,20,120,80)
 $event.Graphics.FillRectangle([Drawing.Brushes]::Orange,($sender.Width-$x-120),110,120,40)
})
$animation=New-Object Windows.Forms.Timer; $animation.Interval=16
$animation.add_Tick({$script:tick++;$panel.Invalidate()});$animation.Start()
$proof=@{copy=$false;paste=$false;cut=$false;text='';done=$false}
$path=$Output
$save={ $proof.text=$text.Text; $proof | ConvertTo-Json | Set-Content -Encoding UTF8 $path }
$text.add_KeyDown({param($sender,$event)
 if ($event.Control -and $event.KeyCode -in @('C','V','X')) {
  if($event.KeyCode -eq 'C'){$proof.copy=$true};if($event.KeyCode -eq 'V'){$proof.paste=$true};if($event.KeyCode -eq 'X'){$proof.cut=$true}
  $event.SuppressKeyPress=$true
 }
 & $save
})
$text.add_TextChanged({ & $save })
$timer=New-Object Windows.Forms.Timer; $timer.Interval=$Seconds*1000
$timer.add_Tick({$form.Close()});$timer.Start()
$form.add_Shown({$form.Activate();$text.Focus(); & $save})
[Windows.Forms.Application]::Run($form)
$proof.done=$true; & $save
$animation.Stop();$animation.Dispose();$timer.Dispose();$form.Dispose();[UUFocus]::SetForegroundWindow($previous) | Out-Null
