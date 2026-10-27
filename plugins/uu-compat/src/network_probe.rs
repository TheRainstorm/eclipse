#[derive(Default)]
pub struct NetworkProbe(uu_core::plugin_backend::NetworkProbe);
impl std::ops::Deref for NetworkProbe {type Target=uu_core::plugin_backend::NetworkProbe;fn deref(&self)->&Self::Target{&self.0}}
impl std::ops::DerefMut for NetworkProbe {fn deref_mut(&mut self)->&mut Self::Target{&mut self.0}}
impl NetworkProbe {pub fn reply_at(&mut self,data:&[u8],at:std::time::Instant)->Option<(u32,u32)>{let seq=self.pending_sequence()?;if !crate::control_wire::echo_matches(data,seq){return None;}self.reply_sequence_at(seq,at)}}
