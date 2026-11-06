use super::*;
use std::{sync::atomic::Ordering, time::{Duration,Instant}};

impl State {
    pub(super) fn sync_owned_views(&mut self) {
        // Assign identities before a viewport is shown. Even unopened views
        // participate in deduplication and source ownership.
        for window in &mut self.windows {
            for launch in window.pending.clone() { window.record_launch(&launch,&mut self.next); }
        }
        let mut catalogs=Vec::new();
        let mut virtual_catalogs=Vec::new();
        for window in &mut self.windows {
            for session in window.sessions.iter_mut().filter(|s|!s.app.exit_requested&&!s.app.disconnecting) {
                let Some(view)=window.views.iter_mut().find(|v|v.id==session.id) else{continue;};
                let Some(handle)=&session.app.controller else{continue;};
                if handle.stats.virtual_displays.available.load(Ordering::Acquire) {
                    let inquiry=self.inquiries.entry(session.id).or_insert((Instant::now(),None));
                    if let Some(id)=inquiry.1 {
                        if handle.stats.virtual_displays.take_response(id).is_some() {inquiry.1=None;}
                    }
                    if Instant::now()>=inquiry.0 {
                        inquiry.1=handle.stats.virtual_displays.request(uu_core::native_virtual_display::Request::Inspect).ok();
                        inquiry.0=Instant::now()+Duration::from_secs(5);
                    }
                    if let Some((_,displays))=handle.stats.virtual_displays.snapshot() {
                        virtual_catalogs.push((base_key(&view.launch.target),handle.stats.virtual_displays.catalog_updated(),displays,handle.stats.virtual_displays.slots()));
                    }
                }
                if !view.launch.target.is_native() || !handle.stats.capabilities.allows(uu_core::capabilities::Capability::Displays) {
                    view.label=session.app.display_label();view.discover_initial=false;continue;
                }
                let screens=session.app.remote_screens();
                if screens.screens.is_empty() {continue;}
                // The portal advertises consent/reselect actions, not physical
                // outputs that may be opened lazily without another grant.
                if screens.screens.iter().all(|s|s.kind==uu_core::screen_control::PORTAL_CHOICE_KIND) {
                    view.discover_initial=false;
                    view.label=screens.selected().map(|s|session.app.screen_label(s)).unwrap_or_else(||session.app.display_label());
                    continue;
                }
                catalogs.push((base_key(&view.launch.target),handle.stats.native_displays.catalog_updated(),
                    screens.screens.iter().map(|screen|(screen.clone(),session.app.screen_label(screen))).collect::<Vec<_>>()));
                if let Some(screen)=screens.screens.iter().find(|s|session.app.displays.selected==Some(s.id)) {
                    view.source=Some(DisplayTarget{id:screen.id,source:screen.display_name.clone()});
                    view.label=session.app.screen_label(screen);
                }
            }
        }
        self.inquiries.retain(|id,_|self.windows.iter().any(|w|w.views.iter().any(|v|v.id==*id)));
        catalogs.sort_by_key(|(_,at,_)|*at);
        virtual_catalogs.sort_by_key(|(_,at,_,_)|*at);
        for (device,_,screens) in catalogs {
            for source in self.sources.iter_mut().filter(|s|s.device==device) {source.active=false;}
            let managed=virtual_catalogs.iter().rev().find(|(key,_,_,_)|*key==device);
            for (screen,label) in screens {
                let display=managed.and_then(|(_,_,displays,_)|displays.iter().find(|d|d.source==screen.display_name));
                let managed_id=display.map(|d|d.id);
                let removable=display.is_some_and(|d|matches!(d.owner,uu_core::virtual_display::Owner::Manual));
                let kind=display
                    .map(|d|if d.persistent {SourceKind::Persistent}else{SourceKind::Temporary})
                    .unwrap_or_else(||if managed.is_some_and(|(_,_,_,slots)|slots.iter().any(|s|s.source==screen.display_name)) || screen.kind!=0 {SourceKind::Virtual}else{SourceKind::Physical});
                if let Some(source)=self.sources.iter_mut().find(|s|s.device==device&&s.screen.display_name==screen.display_name) {
                    source.screen=screen;source.label=label;source.active=true;source.kind=kind;source.managed=managed_id;source.removable=removable;
                } else {
                    let id=self.next;self.next+=1;
                    self.sources.push(SourceRecord{id,device:device.clone(),screen,label,kind,active:true,managed:managed_id,removable});
                }
            }
        }
        self.reconcile_discovered_views();
        // An explicit virtual launch owns its eventual source before SDP tells
        // us its name. Wait for that binding instead of adding a competing lazy
        // view as soon as the original connection sees the new desktop output.
        let creating:std::collections::HashSet<_>=self.windows.iter().flat_map(|window| {
            window.views.iter().filter(|view|view.source.is_none()&&view.launch.target.connection.virtual_display().is_some()
                && (window.pending.iter().any(|launch|launch.target.key()==view.launch.target.key())
                    ||window.sessions.iter().any(|session|session.id==view.id&&!session.app.exit_requested&&!session.app.disconnecting
                        &&(session.app.connect_deferred||session.app.controller.as_ref().is_some_and(|handle|!handle.stopped.load(Ordering::Acquire))))))
                .map(|view|base_key(&view.launch.target))
        }).collect();
        // A normal connection owns every source it has ever discovered as a
        // lazy view. A host may publish its catalog in more than one packet;
        // keep adding newly visible sources, but remember names so closing a
        // tab does not make it reappear on the next refresh.
        let anchors:Vec<_>=self.windows.iter().flat_map(|window|window.sessions.iter().filter_map(|session|{
            let target=session.app.stream_target.clone()?;
            let handle=session.app.controller.as_ref()?;
            if session.app.exit_requested||session.app.disconnecting||!target.is_native()||target.connection.is_auxiliary_view()
                ||!handle.stats.capabilities.allows(uu_core::capabilities::Capability::Displays)
                ||session.app.remote_screens().screens.is_empty() {return None;}
            Some((window.id,session.id,target))
        })).collect();
        for (window_id,session_id,target) in anchors {
            if creating.contains(&base_key(&target)) {continue;}
            let Some(index)=self.windows.iter().position(|w|w.id==window_id) else{continue;};
            let Some(source)=self.windows[index].views.iter().find(|v|v.id==session_id).and_then(|v|v.source.clone()) else{continue;};
            let Some(session)=self.windows[index].sessions.iter_mut().find(|s|s.id==session_id) else{continue;};
            let pinned=session.app.pin_display_view(source.clone())||session.app.pin_current_display_view();
            if !pinned {continue;}
            let device=base_key(&target);
            if let Some(view)=self.windows[index].views.iter_mut().find(|v|v.id==session_id) {view.discover_initial=false;}
            let choices:Vec<_>=self.sources.iter().filter(|s|s.device==device&&s.active).cloned().collect();
            for source in choices {
                let key=(device.clone(),source.screen.display_name.clone());
                if self.windows[index].discovered_sources.contains(&key) {continue;}
                self.windows[index].discovered_sources.insert(key);
                if self.source_owned(&source) {continue;}
                if let Ok(launch)=display_launch(&target,&source.screen) {
                    let id=self.windows[index].record_launch(&launch,&mut self.next);
                    let views=&mut self.windows[index].views;
                    let at=views.iter().position(|v|v.id==id).unwrap();
                    let mut view=views.remove(at);
                    view.label=source.label; // Stay unopened until selected.
                    view.discovered=true;
                    // Restoring a selection must not promote it ahead of the
                    // host's earlier displays. Insert only the newly discovered
                    // view; later refreshes leave user drag order untouched.
                    let at=views.iter().position(|v|base_key(&v.launch.target)==device
                        && v.source.as_ref().is_some_and(|s|s.id>source.screen.id)).unwrap_or(views.len());
                    views.insert(at,view);
                }
            }
        }
        let choices:Vec<_>=self.sources.iter().map(|s|(s.device.clone(),SourceChoice {
            id:s.id,label:s.label.clone(),kind:s.kind,active:s.active,owned:self.source_owned(s),
            current_view:None,managed:s.managed,removable:s.removable,
        })).collect();
        let live_windows=self.windows.iter().filter(|w|!w.closing&&!w.views.is_empty()).count();
        for window in &mut self.windows {
            window.merge_available=live_windows>1;
            window.sources.clear();
            for (device,choice) in &choices {
                let at=window.sources.iter().position(|(key,_)|key==device).unwrap_or_else(|| {
                    window.sources.push((device.clone(),Vec::new()));window.sources.len()-1
                });
                let mut choice=choice.clone();
                if let Some(source)=self.sources.iter().find(|s|s.id==choice.id) {
                    choice.current_view=window.views.iter().find(|v|base_key(&v.launch.target)==*device&&v.source.as_ref().is_some_and(|d|d.source==source.screen.display_name)).map(|v|v.id);
                }
                window.sources[at].1.push(choice);
            }
        }
    }
    fn reconcile_discovered_views(&mut self) {
        let requested:std::collections::HashSet<_>=self.windows.iter().flat_map(|window|window.views.iter()
            .filter(|view|!view.discovered).filter_map(|view|view.source.as_ref().map(|source|(base_key(&view.launch.target),source.source.clone())))).collect();
        for window in &mut self.windows {
            let mut retired=Vec::new();
            window.views.retain_mut(|view| {
                let Some(target)=&view.source else{return true;};
                let key=(base_key(&view.launch.target),target.source.clone());
                let source=self.sources.iter().find(|source|source.device==key.0&&source.screen.display_name==key.1);
                let idle=view.id!=window.active&&!window.sessions.iter().any(|session|session.id==view.id)
                    &&!window.pending.iter().any(|launch|launch.target.key()==view.launch.target.key());
                if view.discovered&&idle&&(source.is_some_and(|source|!source.active)||requested.contains(&key)) {
                    retired.push(key);return false;
                }
                if let Some(source)=source.filter(|source|source.active) {view.label.clone_from(&source.label);}
                true
            });
            for key in retired {window.discovered_sources.remove(&key);}
        }
    }
    pub(super) fn source_owned(&self,source:&SourceRecord)->bool {
        self.windows.iter().filter(|w|!w.closing).flat_map(|w|&w.views).any(|view|
            base_key(&view.launch.target)==source.device && view.source.as_ref().is_some_and(|d|
                !d.source.is_empty()&&d.source==source.screen.display_name))
    }
}
