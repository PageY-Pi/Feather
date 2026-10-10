//! Small session/model-style choosers for lazy skills and process-local proxies.
use eframe::egui;
use feather::proxy::{self,Catalog};
use feather_core::provider::transport::{self,Route};
use std::{path::PathBuf,sync::mpsc};
use crate::theme::*;

pub enum Picker { Skills { resources:feather::resources::ResourceLoad,filter:String,cursor:usize }, Proxy { address:String,cursor:usize,changed:bool } }
pub enum Choice { Skill(String), Proxy(Route), Remove(Route), Error(String), Scan }
#[derive(Default)]
pub struct Proxies {
    pub catalog:Catalog,
    pending:Option<mpsc::Receiver<Catalog>>,
    scan_route: Option<Route>,
    pub error:Option<String>,
}
impl Proxies {
    pub fn new(home:&std::path::Path)->Self {
        let catalog=proxy::load(home);transport::set_route(catalog.selected.clone());
        Self{catalog,pending:None,scan_route:None,error:None}
    }
    pub fn scan(&mut self,home:PathBuf,ctx:egui::Context) {
        if self.pending.is_some(){return;}
        let(tx,rx)=mpsc::channel();self.pending=Some(rx);
        self.scan_route=Some(self.catalog.selected.clone());
        std::thread::spawn(move|| {let found=proxy::discover_for(&home);let catalog=proxy::merge_discovered(&home,found);let _=tx.send(catalog);ctx.request_repaint();});
    }
    pub fn poll(&mut self) {
        let Some(rx)=&self.pending else{return;};
        if let Ok(mut catalog)=rx.try_recv(){
            if self.scan_route.as_ref()!=Some(&self.catalog.selected){catalog.selected=self.catalog.selected.clone();}
            self.scan_route=None;self.error=catalog.error.clone();self.catalog=catalog;
            transport::set_route(self.catalog.selected.clone());self.pending=None;
        }
    }
    pub fn scanning(&self)->bool {self.pending.is_some()}
    pub fn label(&self)->String {
        match &self.catalog.selected {Route::Direct=>"Proxy: Off".into(),Route::Proxy{url}=>format!("Proxy: {}",url.strip_prefix("http://").or_else(||url.strip_prefix("socks5h://")).unwrap_or(url))}
    }
}
pub fn window(ctx:&egui::Context,picker:&mut Picker,proxies:&Proxies,open:&mut bool)->Option<Choice> {
    let mut choice=None;
    let title=if matches!(picker,Picker::Skills{..}){"Choose a skill"}else{"Choose proxy"};
    egui::Window::new(title).open(open).anchor(egui::Align2::CENTER_CENTER,egui::Vec2::ZERO).order(egui::Order::Foreground)
        .collapsible(false).resizable(false).default_width(480.0).max_width((ctx.screen_rect().width()-80.0).max(280.0))
        .show(ctx,|ui|match picker {
            Picker::Skills{resources,filter,cursor}=>{
                ui.label(egui::RichText::new("Only descriptions are listed. Instructions load when a skill is invoked.").small().color(MUTED));
                ui.add_space(8.0);ui.add_sized([ui.available_width(),30.0],egui::TextEdit::singleline(filter).hint_text("Search skills…"));
                let query=filter.to_lowercase();
                let skills:Vec<_>=resources.skills.iter().filter(|s|s.name.to_lowercase().contains(&query)||s.description.to_lowercase().contains(&query)).collect();
                if skills.is_empty(){ui.label("No skills found. Add .feather/skills/<name>/SKILL.md.");}
                else {
                    *cursor=(*cursor).min(skills.len()-1);
                    if ui.input_mut(|i|i.consume_key(egui::Modifiers::NONE,egui::Key::ArrowDown)){*cursor=(*cursor+1)%skills.len();}
                    if ui.input_mut(|i|i.consume_key(egui::Modifiers::NONE,egui::Key::ArrowUp)){*cursor=(*cursor+skills.len()-1)%skills.len();}
                    if ui.input_mut(|i|i.consume_key(egui::Modifiers::NONE,egui::Key::Enter)){choice=Some(Choice::Skill(skills[*cursor].name.clone()));}
                    egui::ScrollArea::vertical().max_height(340.0).show(ui,|ui|for (n,skill) in skills.iter().enumerate(){
                        if ui.add_sized([ui.available_width(),40.0],egui::SelectableLabel::new(n==*cursor,egui::RichText::new(format!("{} · {}",skill.name,skill.description)).color(INK)))
                            .on_hover_text(skill.file_path.display().to_string()).clicked(){choice=Some(Choice::Skill(skill.name.clone()));}
                    });
                }
                for d in resources.diagnostics.iter().take(3){ui.label(egui::RichText::new(&d.message).small().color(MUTED));}
            },
            Picker::Proxy{address,cursor,changed}=>{
                ui.label(egui::RichText::new("Applies to Feather requests only. No VPN or system settings are changed.").small().color(MUTED));
                ui.label(egui::RichText::new("↑ / ↓ select · Enter use · DEL removes saved proxy · Esc closes").small().color(MUTED));
                ui.add_space(8.0);
                let routes:Vec<_>=std::iter::once(Route::Direct).chain(proxies.catalog.entries.iter().map(|e|e.endpoint.route.clone())).collect();
                *cursor=(*cursor).min(routes.len()-1);
                if ui.input_mut(|i|i.consume_key(egui::Modifiers::NONE,egui::Key::ArrowDown)){*cursor=(*cursor+1)%routes.len();}
                if ui.input_mut(|i|i.consume_key(egui::Modifiers::NONE,egui::Key::ArrowUp)){*cursor=(*cursor+routes.len()-1)%routes.len();}
                if !ui.ctx().wants_keyboard_input()&&ui.input_mut(|i|i.consume_key(egui::Modifiers::NONE,egui::Key::Enter)){choice=Some(Choice::Proxy(routes[*cursor].clone()));}
                if !ui.ctx().wants_keyboard_input()&&ui.input_mut(|i|i.consume_key(egui::Modifiers::NONE,egui::Key::Delete))&&matches!(routes[*cursor],Route::Proxy{..}){choice=Some(Choice::Remove(routes[*cursor].clone()));}
                egui::ScrollArea::vertical().max_height(300.0).show(ui,|ui| {
                    for(n,route) in routes.iter().enumerate(){
                        let label=if n==0{"Off · direct connection".into()}else{let entry=&proxies.catalog.entries[n-1];format!("{} · {}",route,if entry.active{"detected"}else{"saved"})};
                        let label=format!("{label}{}",if route==&proxies.catalog.selected{" · current"}else{""});
                        if ui.add_sized([ui.available_width(),36.0],egui::SelectableLabel::new(n==*cursor,label)).clicked(){choice=Some(Choice::Proxy(route.clone()));}
                    }
                });
                if let Some(selected)=routes.get(*cursor) {
                    if matches!(selected,Route::Proxy{..})&&ui.button("Remove highlighted saved proxy").clicked(){choice=Some(Choice::Remove(selected.clone()));}
                }
                ui.add_space(10.0);
                if ui.add_enabled(!proxies.scanning(),egui::Button::new(if proxies.scanning(){"Scanning localhost…"}else{"Scan common proxy ports"})).clicked(){choice=Some(Choice::Scan);}
                let edit=ui.add_sized([ui.available_width(),30.0],egui::TextEdit::singleline(address).hint_text("http://127.0.0.1:7890 or socks5h://127.0.0.1:1080"));
                if edit.changed(){*changed=true;}
                if *changed&&edit.lost_focus()&&ui.input(|i|i.key_pressed(egui::Key::Enter)) {choice=Some(match Route::proxy(address.as_str()){Ok(route)=>Choice::Proxy(route),Err(e)=>Choice::Error(e.to_string())});}
                if ui.button("Add and use").clicked(){choice=Some(match Route::proxy(address.as_str()){Ok(route)=>Choice::Proxy(route),Err(e)=>Choice::Error(e.to_string())});}
                if let Some(error)=&proxies.error{ui.label(egui::RichText::new(error).small().color(RED));}
            },
        });
    choice
}
