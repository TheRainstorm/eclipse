//! Bounded AMF input ownership: current output AND the exact input's release.
//! Other asynchronous encoders do not share this contract.
pub(super) trait Released { fn released(&self) -> bool; }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Key { generation:u64, serial:u64, slot:usize }
struct Flight<L> { key:Key, pts:i64, output:bool, release:L }
struct Slot<T,L> { target:T, flight:Option<Flight<L>> }
pub(super) struct Pool<T,L> {
    generation:u64, serial:u64, slots:Vec<Slot<T,L>>, retired:bool,
    pub allocations:u64, pub reuses:u64, pub submissions:u64,
}
impl<T,L:Released> Pool<T,L> {
    pub fn new(generation:u64)->Self {Self{generation,serial:0,slots:Vec::new(),retired:false,allocations:0,reuses:0,submissions:0}}
    pub fn acquire(&mut self,pts:i64,release:L,create:impl FnOnce()->Result<T,String>)->Result<Key,String> {
        if self.retired {return Err("AMF input generation is retired".into());}
        for slot in &mut self.slots {
            if slot.flight.as_ref().is_some_and(|f|f.output && f.release.released()) {slot.flight=None;}
        }
        let index=if let Some(i)=self.slots.iter().position(|s|s.flight.is_none()) {
            self.reuses+=1;i
        }else if self.slots.len()<2 {
            let target=create()?;self.slots.push(Slot{target,flight:None});self.allocations+=1;self.slots.len()-1
        }else{return Err("both AMF input slots are still leased; refusing overwrite".into());};
        self.serial=self.serial.checked_add(1).ok_or("AMF input lease serial exhausted")?;
        let key=Key{generation:self.generation,serial:self.serial,slot:index};
        self.slots[index].flight=Some(Flight{key,pts,output:false,release});self.submissions+=1;Ok(key)
    }
    pub fn target(&self,key:Key)->Result<(&T,&L),String> {
        let slot=self.slots.get(key.slot).ok_or("unknown AMF input slot")?;
        let f=slot.flight.as_ref().filter(|f|f.key==key).ok_or("stale AMF input lease")?;
        Ok((&slot.target,&f.release))
    }
    pub fn complete(&mut self,key:Key,pts:i64)->Result<(),String> {
        let f=self.slots.get_mut(key.slot).and_then(|s|s.flight.as_mut()).filter(|f|f.key==key).ok_or("stale AMF output lease")?;
        if f.pts!=pts || f.output {self.retired=true;return Err("AMF output PTS/lease mismatch or duplicate completion".into());}
        f.output=true;Ok(())
    }
    pub fn retire(&mut self) {self.retired=true;}
    pub fn pending(&self)->usize {self.slots.iter().filter(|s|s.flight.as_ref().is_some_and(|f|!f.release.released())).count()}
    pub fn slots(&self)->usize {self.slots.len()}
    pub fn generation(&self)->u64 {self.generation}
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc,atomic::{AtomicBool,Ordering}};
    #[derive(Clone,Default)] struct Release(Arc<AtomicBool>);
    impl Release {fn finish(&self){self.0.store(true,Ordering::Release);}}
    impl Released for Release {fn released(&self)->bool{self.0.load(Ordering::Acquire)}}
    #[test] fn output_alone_and_release_alone_never_allow_overwrite() {
        let mut p=Pool::new(1);let a=Release::default();let b=Release::default();
        let ka=p.acquire(1,a.clone(),||Ok(0)).unwrap();p.complete(ka,1).unwrap();
        let kb=p.acquire(2,b.clone(),||Ok(1)).unwrap();b.finish();
        assert!(p.acquire(3,Release::default(),||Ok(2)).is_err());
        a.finish();let kc=p.acquire(3,Release::default(),||panic!("reuse must not allocate")).unwrap();
        assert_ne!(ka,kc);assert!(p.complete(ka,1).is_err());assert!(p.complete(kb,2).is_ok());assert_eq!(p.slots(),2);
    }
    #[test] fn late_and_repeated_release_cannot_free_a_new_submission() {
        let mut p=Pool::new(7);let old=Release::default();let first=p.acquire(1,old.clone(),||Ok(1)).unwrap();
        p.complete(first,1).unwrap();old.finish();let current=p.acquire(2,Release::default(),||Ok(2)).unwrap();old.finish();
        assert_eq!(p.pending(),1);assert!(p.complete(first,1).is_err());assert!(p.target(current).is_ok());
    }
    #[test] fn failure_retires_generation_and_rebuild_ignores_old_callbacks() {
        let mut p=Pool::new(1);let old=Release::default();let old_key=p.acquire(1,old.clone(),||Ok(0)).unwrap();p.retire();
        assert!(p.acquire(2,Release::default(),||Ok(1)).is_err());
        let mut next=Pool::new(2);let current=next.acquire(3,Release::default(),||Ok(2)).unwrap();old.finish();
        assert!(next.complete(old_key,1).is_err());assert_eq!(next.pending(),1);assert!(next.target(current).is_ok());
    }
    #[test] fn bad_pts_duplicate_completion_and_allocation_errors_fail_closed() {
        let mut p:Pool<(),Release>=Pool::new(3);
        assert!(p.acquire(1,Release::default(),||Err("allocation failed".into())).is_err());assert_eq!(p.slots(),0);
        let k=p.acquire(2,Release::default(),||Ok(())).unwrap();assert!(p.complete(k,9).is_err());
        assert!(p.acquire(3,Release::default(),||Ok(())).is_err());
        let mut p=Pool::new(4);let k=p.acquire(1,Release::default(),||Ok(())).unwrap();p.complete(k,1).unwrap();assert!(p.complete(k,1).is_err());
    }
}
