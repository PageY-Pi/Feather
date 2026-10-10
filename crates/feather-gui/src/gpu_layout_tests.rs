use super::*;
#[test]
fn gpu_scale_composer_and_model_footer_never_drift_on_idle_stream_or_caret_frames() {
    for (width,ppp) in [(1160.0,1.25),(610.0,1.25),(610.0,1.5),(1160.0,1.0)] {
        let mut app=tests::fixture();let ctx=app.egui_ctx.clone();theme::install_fonts(&ctx);theme::apply_theme(&ctx);ctx.set_pixels_per_point(ppp);
        app.rows.push(ChatRow::new(RowKind::User,"hello"));app.input="hello 中文".into();
        let mut baseline=None;
        for n in 0..140 {
            app.running=n%2==0;
            if n>20 {app.metrics.begin_turn(n as f64);app.metrics.record_delta(&"some text".repeat((n%30)+1),n as f64);}
            app.last_usage.output=if n>20{(n*20) as u64}else{0};
            let out=ctx.run(egui::RawInput {screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,820.0))),time:Some(n as f64*0.1),..Default::default()},|ctx|app.frame_ui(ctx));
            if n==0{continue;}
            fn collect(shape:&egui::Shape,out:&mut Vec<egui::epaint::RectShape>){match shape{egui::Shape::Rect(r)=>out.push(r.clone()),egui::Shape::Vec(v)=>for s in v{collect(s,out)},_=>{}}}
            let mut rects=Vec::new();for s in &out.shapes{collect(&s.shape,&mut rects);}
            let composer=rects.iter().find(|r|r.fill==PANEL_2&&r.rounding.nw==22.0&&r.rect.width()>300.0).map(|r|r.rect).unwrap();
            let model_y=out.shapes.iter().find_map(|s|match &s.shape{egui::Shape::Text(t) if t.galley.text()=="claude-sonnet-4-5"=>Some(t.pos.y),_=>None}).unwrap();
            let geometry=(composer.top(),composer.bottom(),model_y);
            if n==19{baseline=Some(geometry);}
            if n>19{let old=baseline.unwrap();assert!((geometry.0-old.0).abs()<0.4&&(geometry.1-old.1).abs()<0.4&&(geometry.2-old.2).abs()<0.4,"width={width} ppp={ppp} frame={n} geometry={geometry:?} baseline={old:?}");}
        }
    }
}
