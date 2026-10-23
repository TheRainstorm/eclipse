//! A private password credential for PAKE. The clear-text password is never
//! stored, sent, or included in configuration snapshots and diagnostics.
use crate::device_trust::{Permissions,TrustStore,read_private};
use anyhow::{ensure,Result};
use rand::RngCore;
use ring::pbkdf2;
use serde::{Deserialize,Serialize};
use std::num::NonZeroU32;
use zeroize::{Zeroize,Zeroizing};

const ITERATIONS:u32=600_000;
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    version:u32,
    pub salt:[u8;16],
    verifier:[u8;32],
    pub permissions:Permissions,
}
impl Drop for Record {fn drop(&mut self){self.verifier.zeroize();}}
impl Record {
    pub fn key(&self)->Zeroizing<Vec<u8>> {Zeroizing::new(self.verifier.to_vec())}
    fn validate(&self)->Result<()> {
        ensure!(self.version==1,"配对密码配置版本无效，原文件已保留");
        self.permissions.validate()
    }
}
pub(crate) fn derive(password:&str,salt:&[u8;16])->Result<Zeroizing<Vec<u8>>> {
    ensure!((8..=256).contains(&password.len()),"配对密码须为 8–256 字节");
    let mut output=Zeroizing::new(vec![0;32]);
    pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA256,NonZeroU32::new(ITERATIONS).unwrap(),salt,password.as_bytes(),&mut output);
    Ok(output)
}
pub(crate) fn read(store:&TrustStore)->Result<Option<Record>> {
    let path=store.directory().join("pairing-password.json");
    if !path.try_exists()? {return Ok(None);}
    let record:Record=serde_json::from_slice(&read_private(&path)?)
        .map_err(|_|anyhow::anyhow!("配对密码配置损坏，原文件已保留"))?;
    record.validate()?;
    Ok(Some(record))
}
impl TrustStore {
    /// Safe to show in UI: this never returns the verifier or salt.
    pub fn pairing_password_permissions(&self)->Result<Option<Permissions>> {
        Ok(read(self)?.map(|record|record.permissions))
    }
    /// Caller should run derivation and disk I/O outside the UI thread.
    pub fn set_pairing_password(&self,password:&str,permissions:Permissions)->Result<()> {
        permissions.validate()?;
        let mut salt=[0;16];rand::rngs::OsRng.fill_bytes(&mut salt);
        let derived=derive(password,&salt)?;
        let mut verifier=[0;32];verifier.copy_from_slice(&derived);
        let record=Record{version:1,salt,verifier,permissions};
        let bytes=Zeroizing::new(serde_json::to_vec(&record)?);
        let _lock=Self::lock_at(self.directory())?;
        crate::private_file::write_atomic(&self.directory().join("pairing-password.json"),&bytes)?;
        Ok(())
    }
    pub fn clear_pairing_password(&self)->Result<()> {
        let _lock=Self::lock_at(self.directory())?;
        let path=self.directory().join("pairing-password.json");
        if path.try_exists()? {std::fs::remove_file(path)?;}
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn password_record_has_random_salt_and_never_stores_password_or_changes_device_grants() {
        let root=std::env::temp_dir().join(format!("eclipse-password-test-{}",uuid::Uuid::new_v4()));
        let store=TrustStore::open(&root).unwrap();
        let password="generated fixture password only";
        store.set_pairing_password(password,Permissions::VIEW).unwrap();
        let first=read(&store).unwrap().unwrap();
        assert_eq!(&first.key()[..],&derive(password,&first.salt).unwrap()[..]);
        assert_ne!(&first.key()[..],&derive("wrong generated password",&first.salt).unwrap()[..]);
        let bytes=read_private(&root.join("pairing-password.json")).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains(password));
        store.set_pairing_password(password,Permissions::CONTROL).unwrap();
        assert_ne!(read(&store).unwrap().unwrap().salt,first.salt);
        assert!(store.peers().unwrap().is_empty());
        store.clear_pairing_password().unwrap();
        assert!(read(&store).unwrap().is_none());
        assert!(store.set_pairing_password("short",Permissions::CONTROL).is_err());
        drop(first);
        std::fs::remove_dir_all(root).unwrap();
    }
}
