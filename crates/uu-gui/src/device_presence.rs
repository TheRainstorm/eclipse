//! Volatile reachability, separate from saved devices and pairing trust.
use eframe::egui;
use sha2::{Digest,Sha256};
use std::{collections::HashMap,sync::{mpsc,Arc},time::{Duration,Instant}};
use uu_core::{device_book::{Book,Destination},Config};

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub(super) enum Status {Online,Offline,#[default] Unknown}
fn aggregate(states:impl IntoIterator<Item=Status>)->Status {
    let mut any=false;let mut unknown=false;
    for state in states {any=true;match state{Status::Online=>return Status::Online,Status::Unknown=>unknown=true,Status::Offline=>{}}}
    if any&&!unknown{Status::Offline}else{Status::Unknown}
}
type Results=HashMap<String,Status>;
struct Job {events:mpsc::Receiver<Results>,cancel:Option<tokio::sync::oneshot::Sender<()>>}
impl Drop for Job {fn drop(&mut self){if let Some(cancel)=self.cancel.take(){let _=cancel.send(());}}}
#[derive(Default)]
pub(super) struct Monitor {
    key:Option<[u8;32]>,job:Option<Job>,results:Results,at:Option<Instant>,next:Option<Instant>,
}
impl Monitor {
    pub fn status(&self,id:&str)->Status {
        if self.at.is_some_and(|at|at.elapsed()<Duration::from_secs(20)){self.results.get(id).copied().unwrap_or_default()}else{Status::Unknown}
    }
    pub fn refresh(&mut self){self.next=None;}
    pub fn poll(&mut self,ctx:&egui::Context,config:Option<&Config>,book:&Book,provider_generation:u64) {
        // Edits and account switches retire the whole old snapshot, including
        // replies already queued by a previous worker.
        let routes:Vec<_>=book.devices.iter().filter(|p|p.redirect.is_none()).map(|p|(&p.id,&p.routes)).collect();
        let key: [u8;32]=Sha256::digest(serde_json::to_vec(&(routes,&book.services,provider_generation)).unwrap_or_default()).into();
        if self.key!=Some(key){*self=Self{key:Some(key),..Default::default()};}
        if let Some(job)=&self.job {
            match job.events.try_recv(){
                Ok(results)=>{self.results=results;self.at=Some(Instant::now());self.job=None;self.next=Some(Instant::now()+Duration::from_secs(5));},
                Err(mpsc::TryRecvError::Disconnected)=>{self.job=None;self.next=Some(Instant::now()+Duration::from_secs(5));},
                Err(mpsc::TryRecvError::Empty)=>{},
            }
        }
        if self.job.is_none() && self.next.is_none_or(|at|Instant::now()>=at) {
            if let Some(config)=config.filter(|_|!book.devices.is_empty()) {
                let config=config.clone();let book=book.clone();let context=ctx.clone();
                let (send,events)=mpsc::channel();let (cancel,cancelled)=tokio::sync::oneshot::channel();
                std::thread::spawn(move|| {
                    let Ok(runtime)=tokio::runtime::Builder::new_current_thread().enable_all().build() else{return;};
                    runtime.block_on(async{tokio::select!{
                        _=cancelled=>{},
                        results=probe(config,book)=>{let _=send.send(results);context.request_repaint();},
                    }});
                    runtime.shutdown_background();
                });
                self.job=Some(Job{events,cancel:Some(cancel)});
            }
        }
        ctx.request_repaint_after(Duration::from_millis(if self.job.is_some(){100}else{1000}));
    }
}
async fn probe(config:Config,book:Book)->Results {
    let profiles:Vec<_>=book.devices.iter().filter(|p|p.redirect.is_none()).collect();
    let mut routes:HashMap<String,Status>=profiles.iter().flat_map(|p|p.routes.iter()).map(|r|(r.id.clone(),Status::Unknown)).collect();
    let slots=Arc::new(tokio::sync::Semaphore::new(8));
    let mut tasks=tokio::task::JoinSet::<Vec<(String,Status)>>::new();
    for route in profiles.iter().flat_map(|p|p.routes.iter()) {
        if let Destination::Ip{address,..}=&route.destination {
            let (id,address,slots)=(route.id.clone(),address.clone(),slots.clone());
            tasks.spawn(async move{
                let Ok(_slot)=slots.acquire_owned().await else{return vec![];};
                let status=match tokio::time::timeout(Duration::from_secs(3),uu_core::native_lan::inspect(&address)).await {
                    Ok(Ok(_))=>Status::Online,_=>Status::Offline,
                };
                vec![(id,status)]
            });
        }
    }
    let mut providers:std::collections::BTreeMap<String,Vec<(String,String)>>=Default::default();
    for route in profiles.iter().flat_map(|p|p.routes.iter()) {
        if let Destination::Plugin{plugin,device}=&route.destination {providers.entry(plugin.clone()).or_default().push((route.id.clone(),device.clone()));}
    }
    for (id,targets) in providers {
        let Ok(provider)=uu_core::plugins::registry().get(&id) else{continue;};
        let slots=slots.clone();let config=config.clone();
        tasks.spawn(async move {
            let Ok(_slot)=slots.acquire_owned().await else{return vec![];};
            let Ok(Ok(devices))=tokio::time::timeout(Duration::from_secs(5),provider.devices(&config)).await else{return vec![];};
            targets.into_iter().map(|(id,target)| {
                let status=devices.iter().find(|d|d.id==target).and_then(|d|d.online).map_or(Status::Unknown,|online|if online{Status::Online}else{Status::Offline});(id,status)
            }).collect()
        });
    }
    for service in &book.services {
        let targets:Vec<_>=profiles.iter().flat_map(|p|p.routes.iter()).filter_map(|r|match &r.destination{
            Destination::Vps{service:id,device} if id==&service.id=>Some((r.id.clone(),device.clone())),_=>None,
        }).collect();
        if targets.is_empty(){continue;}
        let service=service.clone();let slots=slots.clone();
        tasks.spawn(async move {
            let Ok(_slot)=slots.acquire_owned().await else{return vec![];};
            let settings=uu_core::internet::Settings{server:service.address,server_token:service.token_file,server_ca:service.certificate,..Default::default()};
            let Ok(endpoint)=settings.self_hosted() else{return vec![];};
            let mut results=Vec::new();
            for chunk in targets.chunks(128) {
                let devices:Vec<_>=chunk.iter().map(|(_,id)|id.clone()).collect();
                // Query failures (including an older server) leave Unknown.
                if let Ok(Ok(online))=tokio::time::timeout(Duration::from_secs(5),uu_core::internet::self_hosted::presence(&endpoint,&devices)).await {
                    results.extend(chunk.iter().map(|(id,device)|(id.clone(),if online.contains(device){Status::Online}else{Status::Offline})));
                }
            }
            results
        });
    }
    let collect=async {
        while let Some(result)=tasks.join_next().await {if let Ok(values)=result{routes.extend(values);}}
    };
    // A large library or stalled DNS may not hold this snapshot forever.
    let _=tokio::time::timeout(Duration::from_secs(8),collect).await;
    tasks.abort_all();
    profiles.iter().map(|p|(p.id.clone(),aggregate(p.routes.iter().map(|r|routes[&r.id])))).collect()
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn any_live_route_wins_but_unknown_never_becomes_offline(){
        assert_eq!(aggregate([Status::Offline,Status::Online,Status::Unknown]),Status::Online);
        assert_eq!(aggregate([Status::Offline,Status::Offline]),Status::Offline);
        assert_eq!(aggregate([Status::Offline,Status::Unknown]),Status::Unknown);
        assert_eq!(aggregate([]),Status::Unknown);
    }
    #[test] fn provider_and_route_changes_discard_queued_results_and_stale_observations(){
        let config=Config{data_dir:std::env::temp_dir(),state_dir:std::env::temp_dir(),signaling_host:"unused.example.invalid".into()};
        let mut monitor=Monitor::default();let book=Book::default();let ctx=egui::Context::default();
        monitor.poll(&ctx,Some(&config),&book,1);
        let (send,events)=mpsc::channel();send.send(HashMap::from([("old-device".into(),Status::Online)])).unwrap();
        monitor.job=Some(Job{events,cancel:None});
        monitor.poll(&ctx,Some(&config),&book,2);
        assert_eq!(monitor.status("old-device"),Status::Unknown);assert!(monitor.job.is_none());
        monitor.results.insert("old-device".into(),Status::Online);monitor.at=Some(Instant::now()-Duration::from_secs(30));
        assert_eq!(monitor.status("old-device"),Status::Unknown);
    }
    #[tokio::test]
    async fn ip_probe_requires_the_native_service_and_never_starts_pairing_or_media(){
        use uu_core::{device_book::{Profile,Route},native,native_lan};
        let root=std::env::temp_dir().join(format!("presence-owned-{}",uuid::Uuid::new_v4()));
        let (stop,rx)=tokio::sync::watch::channel(false);let (events,mut incoming)=tokio::sync::mpsc::unbounded_channel();
        let (server,handle)=native_lan::Server::bind(native_lan::Config{
            name:"Generated presence host".into(),bind:"127.0.0.1:0".parse().unwrap(),discovery_bind:None,data_dir:root.clone(),
            source:native::CaptureSource::Synthetic{width:64,height:64,fps:30},fps:30,bitrate_kbps:300,
            encoder:native::EncoderPreference::Software,encoder_tuning:Default::default(),video_policy:None,codec:Default::default(),rate_control:Default::default(),allow_input:false,audio:None,clipboard:None,
        },rx,events).await.unwrap();
        let live=server.address().unwrap().to_string();let serving=tokio::spawn(server.run());
        let unrelated=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let wrong=unrelated.local_addr().unwrap().to_string();
        let unrelated_task=tokio::spawn(async move{let (socket,_)=unrelated.accept().await.unwrap();drop(socket);});
        let mut book=Book::default();
        let online=Profile::new("Reachable",Destination::Ip{address:live,legacy_device:None});
        let offline=Profile::new("Wrong service",Destination::Ip{address:wrong,legacy_device:None});
        let mut uncertain=offline.clone();uncertain.id=uuid::Uuid::new_v4().to_string();
        uncertain.routes=vec![Route::new(Destination::Plugin{plugin:"example-addon".into(),device:"generated-unknown".into()})];
        book.devices=vec![online.clone(),offline.clone(),uncertain.clone()];
        let config=Config{data_dir:root.join("client"),state_dir:root.join("state"),signaling_host:"unused.example.invalid".into()};
        let states=probe(config,book).await;
        assert_eq!(states[&online.id],Status::Online);assert_eq!(states[&offline.id],Status::Offline);assert_eq!(states[&uncertain.id],Status::Unknown);
        assert!(incoming.try_recv().is_err(),"presence must not start a session or pairing prompt");
        stop.send_replace(true);serving.await.unwrap().unwrap();unrelated_task.await.unwrap();drop(handle);std::fs::remove_dir_all(root).unwrap();
    }
}
