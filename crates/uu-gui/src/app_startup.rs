//! Per-user startup registration, changed only by explicitly saving the option.
use anyhow::{Context,Result};
#[cfg(windows)]
pub fn apply(enabled:bool)->Result<()> {
    use std::{os::windows::ffi::OsStrExt,ptr};
    use windows_sys::Win32::{Foundation::{ERROR_FILE_NOT_FOUND,ERROR_SUCCESS},System::Registry::*};
    let wide=|text:&std::ffi::OsStr| text.encode_wide().chain(Some(0)).collect::<Vec<_>>();
    let path=wide(std::ffi::OsStr::new("Software\\Microsoft\\Windows\\CurrentVersion\\Run"));
    let name=wide(std::ffi::OsStr::new("uu-client"));
    let mut key=ptr::null_mut();
    let status=unsafe {RegCreateKeyExW(HKEY_CURRENT_USER,path.as_ptr(),0,ptr::null(),0,KEY_SET_VALUE,ptr::null(),&mut key,ptr::null_mut())};
    anyhow::ensure!(status==ERROR_SUCCESS,"无法打开当前用户的启动设置（{status}）");
    struct Key(HKEY);impl Drop for Key {fn drop(&mut self){unsafe{RegCloseKey(self.0);}}}
    let key=Key(key);
    let status=if enabled {
        let exe=std::env::current_exe().context("无法确定软件位置")?;
        let mut value=vec![b'"' as u16];value.extend(exe.as_os_str().encode_wide());value.extend([b'"' as u16,0]);
        unsafe {RegSetValueExW(key.0,name.as_ptr(),0,REG_SZ,value.as_ptr().cast(),(value.len()*2) as u32)}
    }else{unsafe {RegDeleteValueW(key.0,name.as_ptr())}};
    anyhow::ensure!(status==ERROR_SUCCESS || (!enabled && status==ERROR_FILE_NOT_FOUND),"无法保存软件自启动（{status}）");
    Ok(())
}
#[cfg(target_os="linux")]
pub fn apply(enabled:bool)->Result<()> {
    let directory=std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from)
        .or_else(||std::env::var_os("HOME").map(|h|std::path::PathBuf::from(h).join(".config")))
        .context("无法确定当前用户的设置目录")?.join("autostart");
    let path=directory.join("uu-client.desktop");
    if !enabled {
        match std::fs::remove_file(path) {Ok(())=>{},Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},Err(e)=>return Err(e.into())}
        return Ok(());
    }
    let exe=std::env::current_exe().context("无法确定软件位置")?;
    let exe=exe.to_str().context("软件路径不是有效的 UTF-8")?;
    anyhow::ensure!(!exe.contains(['\n','\r']),"软件路径包含不支持的字符");
    // Desktop Entry Exec escaping is separate from shell escaping. Literal %
    // must be doubled to avoid desktop field-code expansion.
    let exe=exe.replace('\\',"\\\\").replace('"',"\\\"").replace('`',"\\`").replace('$',"\\$").replace('%',"%%");
    let command=format!("\"/usr/bin/env\" \"{exe}\"").replace('\\',"\\\\");
    let content=format!("[Desktop Entry]\nType=Application\nName=uu-client\nExec={command}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n");
    super::settings::save_bytes(&path,content.as_bytes())
}
#[cfg(not(any(windows,target_os="linux")))]
pub fn apply(_enabled:bool)->Result<()> {anyhow::bail!("此平台尚不支持软件自启动")}
