//! Persistent renderer choice + restart handoff. Separate GUI-only config
//! document: choosing a renderer never overwrites model/auth/preferences.
use std::path::Path;
use eframe::egui;
use serde::{Serialize,Deserialize};

#[derive(Debug,Copy,Clone,PartialEq,Eq,Serialize,Deserialize,Default)]
#[serde(rename_all="lowercase")]
pub enum Backend { #[default] Cpu, OpenGl, Wgpu }
impl Backend {

    pub fn status(self)->&'static str {if self==Self::Cpu{"Render: CPU"}else{"Render: GPU"}}
    pub fn env(self)->&'static str {match self{Self::Cpu=>"cpu",Self::OpenGl=>"glow",Self::Wgpu=>"wgpu"}}
    pub fn from_env(value:&str)->Option<Self>{match value.to_ascii_lowercase().as_str(){"cpu"|"software"=>Some(Self::Cpu),"glow"|"opengl"=>Some(Self::OpenGl),"wgpu"|"vulkan"|"dx12"=>Some(Self::Wgpu),_=>None}}
    /// Old saved OpenGL/"glow" selections migrate in memory to native wgpu
    /// on Windows. Never re-enter the WGL path sampled in the stalled process.
    pub fn safe_for_platform(self)->Self {
        if cfg!(windows)&&self==Self::OpenGl {Self::Wgpu}else{self}
    }
    pub fn accelerated()->Self {if cfg!(windows){Self::Wgpu}else{Self::OpenGl}}
}
pub fn load(home:&Path)->Backend {
    feather::config_store::read(home,"gui").ok().flatten()
                .and_then(|v|v.get("renderer").cloned()).and_then(|v|serde_json::from_value(v).ok()).unwrap_or_default()
}
pub fn selected(home:&Path,preview:bool)->Backend {
    std::env::var("FEATHER_GUI_RENDERER").ok().as_deref().and_then(Backend::from_env)
        .unwrap_or_else(||if preview{Backend::Cpu}else{load(home)}).safe_for_platform()
}
#[cfg(test)]
pub fn save(home:&Path,backend:Backend)->std::io::Result<()> {
    feather::config_store::update(home,"gui",|v|v["renderer"]=serde_json::json!(backend))
}
/// Renderer selection lives in a text-only modal, like the model/session picker.
#[derive(Clone)]
pub struct Picker { pub cursor: usize }
impl Picker {
    pub fn new(active: Backend) -> Self { Self { cursor: usize::from(active != Backend::Cpu) } }

}
#[cfg(test)]
fn test_shape_texts(shapes:&[egui::epaint::ClippedShape])->Vec<String> {
    shapes.iter().filter_map(|s|match &s.shape {egui::Shape::Text(t)=>Some(t.galley.text().to_owned()),_=>None}).collect()
}
pub fn trigger(ui: &mut egui::Ui, active: Backend) -> bool {
    let response = ui.add(egui::Label::new(egui::RichText::new(active.status()).size(12.0).color(crate::theme::TEAL))
        .sense(egui::Sense::click()));
    preview_bounds("selector",response.rect);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Choose renderer · CPU uses less memory; GPU uses native graphics acceleration. A change restarts Feather.")
        .clicked()
}
pub fn picker_window(ctx: &egui::Context, picker: &mut Picker, active: Backend, enabled: bool, open: &mut bool) -> Option<Backend> {
    let options=[Backend::Cpu,Backend::accelerated()];
    let mut picked=None;
    egui::Window::new("Choose renderer").title_bar(false)
        .anchor(egui::Align2::CENTER_CENTER,egui::vec2(if ctx.screen_rect().width()<850.0{60.0}else{80.0},32.0))
        .order(egui::Order::Foreground).collapsible(false).resizable(false)
        .default_width(360.0).max_width((ctx.screen_rect().width()*0.48).clamp(260.0,360.0))
        .frame(egui::Frame::window(&ctx.style()).inner_margin(egui::Margin::same(18.0)))
        .show(ctx,|ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Choose renderer").size(19.0).strong().color(crate::theme::INK));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui| {
                    let (rect,_)=ui.allocate_exact_size(egui::vec2(44.0,42.0),egui::Sense::hover());
                    if crate::window_chrome::control(ui,rect,crate::window_chrome::Control::Close).clicked(){*open=false;}
                    let c=rect.center();let stroke=egui::Stroke::new(1.8_f32,crate::theme::INK);
                    ui.painter().line_segment([c-egui::vec2(5.0,5.0),c+egui::vec2(5.0,5.0)],stroke);
                    ui.painter().line_segment([c+egui::vec2(-5.0,5.0),c+egui::vec2(5.0,-5.0)],stroke);
                });
            });
            ui.add_space(10.0);
            ui.label(egui::RichText::new("↑ / ↓ choose · Enter selects · Esc closes").size(12.0).color(crate::theme::MUTED));
            ui.add_space(10.0);
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE,egui::Key::ArrowDown)) {picker.cursor=(picker.cursor+1)%options.len();}
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE,egui::Key::ArrowUp)) {picker.cursor=(picker.cursor+options.len()-1)%options.len();}
            ui.add_enabled_ui(enabled,|ui| {
                for (index,backend) in options.into_iter().enumerate() {
                    let selected=index==picker.cursor;
                    let text=match backend {Backend::Cpu=>"CPU · lower memory",_=>"GPU · native graphics acceleration"};
                    let response=ui.add_sized([ui.available_width(),40.0],egui::Button::new(
                        egui::RichText::new(format!("{text}{}",if backend==active {"  · current"}else{""})).color(crate::theme::INK)).fill(if selected{crate::theme::SOFT_ACCENT}else{crate::theme::PANEL_2}).stroke(egui::Stroke::new(1.0_f32,if selected{crate::theme::TEAL}else{crate::theme::BORDER})).rounding(10.0));
                    if backend==active {
                        let center=egui::pos2(response.rect.left()+16.0,response.rect.center().y);
                        ui.painter().circle_filled(center,3.0,crate::theme::TEAL);
                    }
                    preview_bounds(backend.env(),response.rect);
                    if response.hovered() && ui.input(|i|i.events.iter().any(|e|matches!(e,egui::Event::PointerMoved(_)))) {picker.cursor=index;}
                    if response.clicked() {picked=Some(backend);}
                    if index+1<options.len(){ui.add_space(6.0);}
                }
                if enabled && ui.input_mut(|i|i.consume_key(egui::Modifiers::NONE,egui::Key::Enter)) {picked=Some(options[picker.cursor]);}
            });
            // Choice rows fill only the modal; descriptions wrap naturally at
            // compact widths instead of forcing an oversized dialog.
            ui.add_space(16.0);
            ui.label(egui::RichText::new(if enabled {"Changing renderer restarts Feather. Your session and unsent draft are kept."}else{"Finish or stop the response and pending sign-in before changing renderer."})
                .size(12.0).color(crate::theme::MUTED));
        });
    picked
}
fn preview_bounds(name:&str,rect:egui::Rect){
    if std::env::var("FEATHER_UI_PREVIEW").ok().as_deref()!=Some("1"){return;}
    if let Ok(dir)=std::env::var("FEATHER_UI_PREVIEW_RENDERER_HITS"){
        let _=std::fs::create_dir_all(&dir);
        let _=std::fs::write(Path::new(&dir).join(format!("{name}.txt")),format!("{} {}",rect.center().x,rect.center().y));
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn legacy_wgl_selection_cannot_reenter_windows_gl(){
        if cfg!(windows){assert_eq!(Backend::OpenGl.safe_for_platform(),Backend::Wgpu);assert_eq!(Backend::accelerated(),Backend::Wgpu);}
        assert_eq!(Backend::Cpu.safe_for_platform(),Backend::Cpu);
    }
    #[test]
    fn cpu_default_and_renderer_only_patch_preserve_other_config(){
        let home=std::env::temp_dir().join(feather_core::provider::gen_id("renderer-test"));
        assert_eq!(load(&home),Backend::Cpu);
        feather::config_store::write(&home,"gui",&serde_json::json!({"other":17})).unwrap();
        feather::config_store::write(&home,"settings",&serde_json::json!({"model":"synthetic/model","thinking":"high"})).unwrap();
        save(&home,Backend::OpenGl).unwrap();assert_eq!(load(&home),Backend::OpenGl);
        assert_eq!(feather::config_store::read(&home,"gui").unwrap().unwrap()["other"],17);
        assert_eq!(feather::config_store::read(&home,"settings").unwrap().unwrap()["model"],"synthetic/model");
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn display_labels_are_explicit(){
        assert_eq!(Backend::Cpu.status(),"Render: CPU");assert_eq!(Backend::OpenGl.status(),"Render: GPU");
        assert_eq!(Backend::from_env("OpenGL"),Some(Backend::OpenGl));
    }
    #[test]
    fn picker_keyboard_selects_or_disables_backend_without_a_persistent_combo() {
        let ctx=egui::Context::default();crate::theme::apply_theme(&ctx);
        let mut picker=Picker::new(Backend::Cpu);let mut open=true;
        for _ in 0..3 {let _=ctx.run(Default::default(),|ctx|{assert_eq!(picker_window(ctx,&mut picker,Backend::Cpu,true,&mut open),None);});}
        let key=|key|egui::Event::Key{key,physical_key:None,pressed:true,repeat:false,modifiers:egui::Modifiers::NONE};
        let mut picked=None;
        let out=ctx.run(egui::RawInput{events:vec![key(egui::Key::ArrowDown),key(egui::Key::Enter)],..Default::default()},|ctx|{picked=picker_window(ctx,&mut picker,Backend::Cpu,true,&mut open);});
        assert_eq!(picked,Some(Backend::accelerated()));
        assert!(test_shape_texts(&out.shapes).iter().any(|s|s=="Choose renderer"));
        let _=ctx.run(egui::RawInput{events:vec![key(egui::Key::Enter)],..Default::default()},|ctx|{picked=picker_window(ctx,&mut picker,Backend::Cpu,false,&mut open);});
        assert_eq!(picked,None);
    }
}
