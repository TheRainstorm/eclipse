//! Give local signed-in users only the process right needed to read the SCM
//! supervisor's actual image path. File, token and administrator-pipe ACLs stay
//! independent. Existing process ACEs, including explicit denials, are merged.
use super::*;

struct Local(*mut core::ffi::c_void);
impl Drop for Local {
    fn drop(&mut self) { unsafe { LocalFree(self.0); } }
}
fn check(code: u32) -> Result<()> {
    if code == ERROR_SUCCESS { Ok(()) }
    else { Err(std::io::Error::from_raw_os_error(code as i32).into()) }
}
fn authenticated_users() -> Result<[usize; 16]> {
    let mut sid = [0usize; 16];
    let mut length = std::mem::size_of_val(&sid) as u32;
    winok(unsafe { CreateWellKnownSid(WinAuthenticatedUserSid, ptr::null_mut(), sid.as_mut_ptr().cast(), &mut length) })?;
    Ok(sid)
}
fn grant_limited_query(process: HANDLE) -> Result<()> {
    let mut descriptor = ptr::null_mut();
    let mut existing = ptr::null_mut();
    check(unsafe { GetSecurityInfo(process, SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION,
        ptr::null_mut(), ptr::null_mut(), &mut existing, ptr::null_mut(), &mut descriptor) })?;
    let _descriptor = Local(descriptor);
    ensure!(!existing.is_null(), "Cannot merge a missing process DACL");
    ensure!(unsafe { IsValidAcl(existing) } != 0, "Invalid process DACL");
    let mut sid = authenticated_users()?;
    let mut info: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
    winok(unsafe { GetAclInformation(existing, (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
        std::mem::size_of_val(&info) as u32, AclSizeInformation) })?;
    let mut entries = Vec::new();
    for index in 0..info.AceCount {
        let mut entry = ptr::null_mut();
        winok(unsafe { GetAce(existing, index, &mut entry) })?;
        let header = unsafe { &*entry.cast::<ACE_HEADER>() };
        ensure!(usize::from(header.AceSize) >= std::mem::size_of::<ACE_HEADER>(), "Invalid process ACE");
        entries.push((entry, u32::from(header.AceSize)));
    }
    let size = info.AclBytesInUse as usize + std::mem::size_of::<ACCESS_ALLOWED_ACE>()
        - std::mem::size_of::<u32>() + unsafe { GetLengthSid(sid.as_mut_ptr().cast()) } as usize;
    ensure!(size <= u16::MAX as usize, "Process DACL is too large");
    let mut storage = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
    let merged = storage.as_mut_ptr().cast::<ACL>();
    winok(unsafe { InitializeAcl(merged, size as u32, ACL_REVISION_DS) })?;
    // SetEntriesInAcl(GRANT_ACCESS) can replace this trustee's existing deny.
    // Copy all old ACE bytes in the same order, then append the new allow. This
    // also keeps inherited and group denials ahead of the newly granted right;
    // canonicalizing this ACL would change their existing policy precedence.
    for (entry, bytes) in entries {
        winok(unsafe { AddAce(merged, ACL_REVISION_DS, u32::MAX, entry, bytes) })?;
    }
    winok(unsafe { AddAccessAllowedAceEx(merged, ACL_REVISION_DS, 0,
        PROCESS_QUERY_LIMITED_INFORMATION, sid.as_mut_ptr().cast()) })?;
    check(unsafe { SetSecurityInfo(process, SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION,
        ptr::null_mut(), ptr::null_mut(), merged, ptr::null_mut()) })
}

/// Called by the SCM supervisor on itself, never on another process or its
/// media child. Failure is diagnostic and must not stop background sharing.
pub fn enable_running_version_query() -> Result<()> {
    grant_limited_query(unsafe { GetCurrentProcess() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
    }
    struct Impersonation;
    impl Drop for Impersonation {
        fn drop(&mut self) { unsafe { RevertToSelf(); } }
    }
    fn acl_entries(process: HANDLE) -> Vec<(u8, u8, u32)> {
        let mut descriptor=ptr::null_mut();let mut acl=ptr::null_mut();
        check(unsafe{GetSecurityInfo(process,SE_KERNEL_OBJECT,DACL_SECURITY_INFORMATION,ptr::null_mut(),ptr::null_mut(),&mut acl,ptr::null_mut(),&mut descriptor)}).unwrap();
        let _descriptor=Local(descriptor);
        let mut info:ACL_SIZE_INFORMATION=unsafe{std::mem::zeroed()};
        winok(unsafe{GetAclInformation(acl,(&mut info as *mut ACL_SIZE_INFORMATION).cast(),std::mem::size_of_val(&info) as u32,AclSizeInformation)}).unwrap();
        (0..info.AceCount).map(|i|{let mut ace=ptr::null_mut();winok(unsafe{GetAce(acl,i,&mut ace)}).unwrap();let ace=unsafe{&*ace.cast::<ACCESS_ALLOWED_ACE>()};(ace.Header.AceType,ace.Header.AceFlags,ace.Mask)}).collect()
    }
    #[test]
    fn restricted_reader_can_query_owned_image_but_cannot_read_memory_or_control_it() {
        let child = Child(std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 20"])
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()).creation_flags(CREATE_NO_WINDOW).spawn().unwrap());
        let process = child.0.as_raw_handle();
        let own_sid = process_user(unsafe { GetCurrentProcess() }).unwrap();
        // The fixture is ours. Start from a known non-public DACL while keeping
        // this user's cleanup handle valid; no existing service is touched.
        let mut raw = ptr::null_mut();
        winok(unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide(format!("D:P(A;;GA;;;SY)(A;;GA;;;{own_sid})")).as_ptr(), SDDL_REVISION_1,
            &mut raw, ptr::null_mut()) }).unwrap();
        let _raw = Local(raw);
        let mut present = 0; let mut defaulted = 0; let mut acl = ptr::null_mut();
        winok(unsafe { GetSecurityDescriptorDacl(raw, &mut present, &mut acl, &mut defaulted) }).unwrap();
        check(unsafe { SetSecurityInfo(process, SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION,
            ptr::null_mut(), ptr::null_mut(), acl, ptr::null_mut()) }).unwrap();
        grant_limited_query(process).unwrap();
        let mut original = ptr::null_mut();
        winok(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY, &mut original) }).unwrap();
        let original = Handle(original);
        let mut sid = authenticated_users().unwrap();
        let mut world = [0usize; 16];let mut world_length=std::mem::size_of_val(&world) as u32;
        winok(unsafe { CreateWellKnownSid(WinWorldSid, ptr::null_mut(), world.as_mut_ptr().cast(), &mut world_length) }).unwrap();
        let restrictions = [SID_AND_ATTRIBUTES { Sid: sid.as_mut_ptr().cast(), Attributes: 0 },
            SID_AND_ATTRIBUTES { Sid: world.as_mut_ptr().cast(), Attributes: 0 }];
        let mut restricted = ptr::null_mut();
        winok(unsafe { CreateRestrictedToken(original.0, DISABLE_MAX_PRIVILEGE, 0, ptr::null(),
            0, ptr::null(), restrictions.len() as u32, restrictions.as_ptr(), &mut restricted) }).unwrap();
        let restricted = Handle(restricted);
        let mut token = ptr::null_mut();
        winok(unsafe { DuplicateTokenEx(restricted.0, TOKEN_QUERY | TOKEN_IMPERSONATE,
            ptr::null(), SecurityImpersonation, TokenImpersonation, &mut token) }).unwrap();
        let token = Handle(token);
        winok(unsafe { SetThreadToken(ptr::null(), token.0) }).unwrap();
        let impersonation = Impersonation;
        let limited = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, child.0.id()) });
        assert!(!limited.0.is_null(), "{}", std::io::Error::last_os_error());
        let mut image = [0u16; 32768]; let mut length = image.len() as u32;
        winok(unsafe { QueryFullProcessImageNameW(limited.0, 0, image.as_mut_ptr(), &mut length) }).unwrap();
        assert!(length > 0);
        for access in [PROCESS_VM_READ, PROCESS_VM_WRITE, PROCESS_TERMINATE, PROCESS_DUP_HANDLE] {
            let denied = Handle(unsafe { OpenProcess(access, 0, child.0.id()) });
            assert!(denied.0.is_null(), "unexpected process access {access:#x}");
        }
        drop(impersonation);
        drop(limited);
        // Merging read access must also respect an administrator's explicit
        // deny. This remains an owned child object, not a system service.
        for denial in [
            format!("D:P(D;;0x1000;;;AU)(A;;GA;;;SY)(A;;GA;;;{own_sid})"),
            format!("D:P(A;;GA;;;SY)(A;;GA;;;{own_sid})(D;ID;0x1000;;;AU)"),
            format!("D:P(A;;GA;;;SY)(A;;GA;;;{own_sid})(D;ID;0x1000;;;WD)"),
        ] {
            let mut deny_descriptor = ptr::null_mut();
            winok(unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(denial).as_ptr(), SDDL_REVISION_1, &mut deny_descriptor, ptr::null_mut()) }).unwrap();
            let _deny_descriptor = Local(deny_descriptor);
            winok(unsafe { GetSecurityDescriptorDacl(deny_descriptor, &mut present, &mut acl, &mut defaulted) }).unwrap();
            check(unsafe { SetSecurityInfo(process, SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION,
                ptr::null_mut(), ptr::null_mut(), acl, ptr::null_mut()) }).unwrap();
            let before = acl_entries(process);
            grant_limited_query(process).unwrap();
            let after = acl_entries(process);
            assert_eq!(&after[..before.len()], &before, "old ACE order, masks and flags must be unchanged");
            winok(unsafe { SetThreadToken(ptr::null(), token.0) }).unwrap();
            let impersonation = Impersonation;
            let denied = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, child.0.id()) });
            assert!(denied.0.is_null(), "an explicit or inherited query deny was bypassed: {before:?}");
            drop(impersonation);
        }
    }
}
