param(
 [Parameter(Mandatory=$true)][string]$Stage,
 [Parameter(Mandatory=$true)][string]$Monitor,
 [ValidateRange(1,600)][int]$Seconds=60,
 [ValidateRange(30,240)][int]$Fps=60
)
$ErrorActionPreference='Stop'
[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false)
$Stage=[IO.Path]::GetFullPath($Stage)
if(!(Test-Path -LiteralPath $Stage -PathType Container)){throw 'Stage must be an existing owned directory'}
foreach($leaf in @('ready.json','scene-report.json')){if(Test-Path -LiteralPath (Join-Path $Stage $leaf)){throw ('Refusing to replace '+$leaf)}}
Add-Type -AssemblyName System.Windows.Forms,System.Drawing,System.Web.Extensions
Add-Type -ReferencedAssemblies System.Windows.Forms,System.Drawing,System.Web.Extensions -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Web.Script.Serialization;
using System.Windows.Forms;

public sealed class EclipseLatencyScene : Form {
 readonly Bitmap[] images=new Bitmap[16];
 readonly Font font=new Font(FontFamily.GenericMonospace,22,FontStyle.Bold,GraphicsUnit.Pixel);
 readonly List<long[]> paints=new List<long[]>();
 int frame;
 Point marker;
 bool hasMarker;
 bool running=true;
 readonly string stage;
 readonly string monitor;
 readonly Rectangle source;
 readonly int fps;
 readonly int seconds;
 long skipped;
 long start;
 double cpuSeconds;
 string reason="deadline";
 string failure;
 bool timerCancelled;
 bool timerClosed;
 bool windowClosed;
 bool resourcesDisposed;

 [StructLayout(LayoutKind.Sequential)] struct NativeRect { public int left,top,right,bottom; }
 [StructLayout(LayoutKind.Sequential)] struct NativePoint { public int x,y; }
 [DllImport("user32.dll",SetLastError=true)] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
 [DllImport("user32.dll")] static extern IntPtr GetThreadDpiAwarenessContext();
 [DllImport("user32.dll")] static extern bool AreDpiAwarenessContextsEqual(IntPtr a,IntPtr b);
 [DllImport("user32.dll",SetLastError=true)] static extern bool SetWindowPos(IntPtr window,IntPtr after,int x,int y,int width,int height,uint flags);
 [DllImport("user32.dll",SetLastError=true)] static extern bool GetWindowRect(IntPtr window,out NativeRect rect);
 [DllImport("user32.dll",SetLastError=true)] static extern bool GetClientRect(IntPtr window,out NativeRect rect);
 [DllImport("user32.dll",SetLastError=true)] static extern bool ClientToScreen(IntPtr window,ref NativePoint point);
 [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr CreateWaitableTimerExW(IntPtr attributes,string name,uint flags,uint access);
 [DllImport("kernel32.dll",SetLastError=true)] static extern bool SetWaitableTimer(IntPtr timer,ref long dueTime,int period,IntPtr completion,IntPtr argument,bool resume);
 [DllImport("kernel32.dll",SetLastError=true)] static extern bool CancelWaitableTimer(IntPtr timer);
 [DllImport("kernel32.dll",SetLastError=true)] static extern bool CloseHandle(IntPtr handle);
 [DllImport("user32.dll",SetLastError=true)] static extern uint MsgWaitForMultipleObjectsEx(uint count,IntPtr[] handles,uint milliseconds,uint wakeMask,uint flags);

 static void WriteJson(string path,object value) {
  var json=new JavaScriptSerializer();json.MaxJsonLength=32*1024*1024;
  string temp=path+".tmp";
  File.WriteAllText(temp,json.Serialize(value),new UTF8Encoding(false));
  if(File.Exists(path))File.Delete(path);
  File.Move(temp,path);
 }
 protected override bool ShowWithoutActivation {get{return true;}}
 protected override CreateParams CreateParams {get{var p=base.CreateParams;p.ExStyle|=0x08000080|0x8;return p;}}
 protected override void OnFormClosing(FormClosingEventArgs e){if(running)reason="window_closed";running=false;base.OnFormClosing(e);}
 protected override void OnMouseMove(MouseEventArgs e){marker=e.Location;hasMarker=true;base.OnMouseMove(e);}
 protected override void OnPaintBackground(PaintEventArgs e){}
 protected override void OnPaint(PaintEventArgs e) {
  long began=Stopwatch.GetTimestamp();
  int w=ClientSize.Width,h=ClientSize.Height;
  e.Graphics.Clear(Color.FromArgb(24,28,36));
  int bandWidth=Math.Max(1,(w-48)/6);
  int bandHeight=Math.Max(32,(h-180)/3);
  for(int band=0;band<6;band++)using(var brush=new SolidBrush(Color.FromArgb(32+(frame*3+band*31)%192,32+(frame+band*41)%192,32+(frame*2+band*23)%192)))
   e.Graphics.FillRectangle(brush,24+band*bandWidth,90,bandWidth-4,bandHeight);
  int tx=Math.Max(24,w-536),ty=Math.Max(120,h-600);
  e.Graphics.DrawImageUnscaled(images[frame%16],tx,ty);
  e.Graphics.DrawString("Eclipse owned 1080p latency scene   frame "+frame+"   "+fps+" Hz",font,Brushes.White,24,24);
  int cell=Math.Max(4,Math.Min(32,(w-48)/24));int by=Math.Max(0,h-2*cell-24);
  for(int bit=0;bit<24;bit++)for(int row=0;row<2;row++) {
   bool lit=((frame>>bit)&1)!=0;if(row==1)lit=!lit;
   e.Graphics.FillRectangle(lit?Brushes.White:Brushes.Black,24+bit*cell,by+row*cell,cell-1,cell-1);
  }
  if(hasMarker) {
   using(var pen=new Pen(Color.Magenta,4)) {
    e.Graphics.DrawEllipse(pen,marker.X-20,marker.Y-20,40,40);
    e.Graphics.DrawLine(pen,marker.X-30,marker.Y,marker.X+30,marker.Y);
    e.Graphics.DrawLine(pen,marker.X,marker.Y-30,marker.X,marker.Y+30);
   }
  }
  if(paints.Count<200000)paints.Add(new long[]{frame,began,Stopwatch.GetTimestamp()});
  base.OnPaint(e);
 }
 protected override void Dispose(bool disposing) {
  if(disposing&&!resourcesDisposed){resourcesDisposed=true;foreach(var image in images)if(image!=null)image.Dispose();font.Dispose();}
  base.Dispose(disposing);
 }
 EclipseLatencyScene(string stage,string monitor,Rectangle source,int seconds,int fps) {
  this.stage=stage;this.monitor=monitor;this.source=source;this.seconds=seconds;this.fps=fps;
  AutoScaleMode=AutoScaleMode.None;FormBorderStyle=FormBorderStyle.None;StartPosition=FormStartPosition.Manual;
  Bounds=source;ShowInTaskbar=false;TopMost=true;Text="Eclipse owned latency scene";
  SetStyle(ControlStyles.UserPaint|ControlStyles.AllPaintingInWmPaint|ControlStyles.OptimizedDoubleBuffer,true);
  for(int phase=0;phase<images.Length;phase++) {
   var image=new Bitmap(512,512,PixelFormat.Format32bppArgb);var pixels=new byte[512*512*4];
   for(int y=0;y<512;y++)for(int x=0;x<512;x++) {
    uint seed=unchecked((uint)((x/2)*73856093)^((uint)(y/2)*19349663)^((uint)phase*83492791)^0x15391417u);
    seed^=seed<<13;seed^=seed>>17;seed^=seed<<5;int i=(y*512+x)*4;
    pixels[i]=(byte)(32+seed%192);pixels[i+1]=(byte)(32+(seed>>8)%192);pixels[i+2]=(byte)(32+(seed>>16)%192);pixels[i+3]=255;
   }
   var bits=image.LockBits(new Rectangle(0,0,512,512),ImageLockMode.WriteOnly,PixelFormat.Format32bppArgb);
   try{Marshal.Copy(pixels,0,bits.Scan0,pixels.Length);}finally{image.UnlockBits(bits);}
   images[phase]=image;
  }
 }
 void CheckGeometry() {
  NativeRect r,c;var origin=new NativePoint();
  if(!GetWindowRect(Handle,out r)||!GetClientRect(Handle,out c)||!ClientToScreen(Handle,ref origin))throw new Win32Exception();
  if(r.left!=source.Left||r.top!=source.Top||r.right!=source.Right||r.bottom!=source.Bottom||origin.x!=source.Left||origin.y!=source.Top||c.right-c.left!=source.Width||c.bottom-c.top!=source.Height)
   throw new InvalidOperationException("Owned scene physical geometry differs from selected monitor");
 }
 void RunScene() {
  IntPtr timer=IntPtr.Zero;
  var process=Process.GetCurrentProcess();double initialCpu=process.TotalProcessorTime.TotalSeconds;
  try {
   timer=CreateWaitableTimerExW(IntPtr.Zero,null,2,0x001F0003);
   if(timer==IntPtr.Zero)throw new Win32Exception(Marshal.GetLastWin32Error(),"High resolution timer unavailable");
   Show();
   if(!SetWindowPos(Handle,new IntPtr(-1),source.X,source.Y,source.Width,source.Height,0x10|0x40))throw new Win32Exception();
   CheckGeometry();
   paints.Clear();start=Stopwatch.GetTimestamp();
   Invalidate();Update();
   WriteJson(Path.Combine(stage,"ready.json"),new {pid=process.Id,hwnd=Handle.ToInt64(),monitor=monitor,screen_rect=new int[]{source.X,source.Y,source.Width,source.Height},frequency_hz=Stopwatch.Frequency,started_qpc=start,requested_fps=fps,seconds=seconds,clock="high_resolution_waitable",no_activation=true,scope="owned scene only; no host process or display changes"});
   long next=1,lastCheck=start;long frequency=Stopwatch.Frequency;
   var handles=new IntPtr[]{timer};
   while(running&&!IsDisposed) {
    long now=Stopwatch.GetTimestamp();
    if(now-start>=(long)seconds*frequency)break;
    if(now-lastCheck>=frequency/10) {
     lastCheck=now;if(File.Exists(Path.Combine(stage,"stop"))){reason="stop_file";break;}
    }
    long target=start+next*frequency/fps;
    long due=-Math.Max(1,(target-now)*10000000L/frequency);
    if(!SetWaitableTimer(timer,ref due,0,IntPtr.Zero,IntPtr.Zero,false))throw new Win32Exception();
    uint result=MsgWaitForMultipleObjectsEx(1,handles,100,0x04FF,4);
    if(result==0) {
     now=Stopwatch.GetTimestamp();if(now-start>=(long)seconds*frequency)break;
     long tick=Math.Max(next,(now-start)*fps/frequency);
     skipped+=Math.Max(0,tick-next);
     if(!File.Exists(Path.Combine(stage,"freeze"))){frame=(int)tick;Invalidate();Update();}
     next=tick+1;
    }
    else if(result==0xFFFFFFFF)throw new Win32Exception();
    Application.DoEvents();
   }
  }
  catch(Exception error){failure=error.ToString();reason="error";throw;}
  finally {
   long ended=Stopwatch.GetTimestamp();cpuSeconds=process.TotalProcessorTime.TotalSeconds-initialCpu;
   if(timer!=IntPtr.Zero){timerCancelled=CancelWaitableTimer(timer);timerClosed=CloseHandle(timer);}
   running=false;if(!IsDisposed){Close();Dispose();}windowClosed=IsDisposed;
   var intervals=new List<double>();var duration=new List<double>();
   for(int i=0;i<paints.Count;i++) {
    duration.Add((paints[i][2]-paints[i][1])*1000.0/Stopwatch.Frequency);
    if(i>0)intervals.Add((paints[i][1]-paints[i-1][1])*1000.0/Stopwatch.Frequency);
   }
   WriteJson(Path.Combine(stage,"scene-report.json"),new {pid=process.Id,monitor=monitor,screen_rect=new int[]{source.X,source.Y,source.Width,source.Height},requested_fps=fps,count=paints.Count,frequency_hz=Stopwatch.Frequency,started_qpc=start,ended_qpc=ended,elapsed_seconds=start==0?0:(ended-start)/(double)Stopwatch.Frequency,actual_paint_fps=paints.Count<2?0:(paints.Count-1)*Stopwatch.Frequency/(double)(paints[paints.Count-1][1]-paints[0][1]),skipped_ticks=skipped,paint_times=paints.ToArray(),paint_columns=new string[]{"frame","paint_begin_qpc","paint_end_qpc"},interval_ms=intervals.ToArray(),paint_duration_ms=duration.ToArray(),cpu_seconds=cpuSeconds,clock="high_resolution_waitable",texture=new {width=512,height=512,phases=16,seed="0x15391417",cell=2},reason=reason,error=failure,cleanup=new {timer_cancelled=timerCancelled,timer_closed=timerClosed,window_closed=windowClosed},no_input_coordinates_logged=true});
  }
 }
 public static void Run(string stage,string monitor,int seconds,int fps) {
  IntPtr previous=SetThreadDpiAwarenessContext(new IntPtr(-4));
  if(previous==IntPtr.Zero||!AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(),new IntPtr(-4)))throw new Win32Exception(Marshal.GetLastWin32Error(),"PMv2 DPI context unavailable");
  try {
   Screen selected=null;foreach(var screen in Screen.AllScreens)if(String.Equals(screen.DeviceName,monitor,StringComparison.OrdinalIgnoreCase))selected=screen;
   if(selected==null)throw new InvalidOperationException("Selected monitor is not active: "+monitor);
   if(selected.Bounds.Width<640||selected.Bounds.Height<600)throw new InvalidOperationException("Selected monitor too small for fixed 512px fixture");
   using(var scene=new EclipseLatencyScene(stage,monitor,selected.Bounds,seconds,fps))scene.RunScene();
  }
  finally{SetThreadDpiAwarenessContext(previous);}
 }
}
'@
try {[EclipseLatencyScene]::Run($Stage,$Monitor,$Seconds,$Fps)}
catch {
 @{error=($_|Out-String);stage=$Stage;monitor=$Monitor;no_display_changes=$true;no_cursor_changes=$true}|ConvertTo-Json -Compress -Depth 6|Set-Content -LiteralPath (Join-Path $Stage 'scene-error.json') -Encoding UTF8
 throw
}
