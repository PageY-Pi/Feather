//! Demand-driven maximum frame rate: active monitor refresh minus one. Never
//! creates a continuous idle animation loop. Read-only native display queries.
use std::time::{Duration,Instant};
pub fn interval(refresh_millihertz:u32)->Duration {
    let hz=if refresh_millihertz>=2000{refresh_millihertz as f64/1000.0}else{60.0};
    Duration::from_secs_f64(1.0/(hz-1.0).max(1.0))
}
pub struct Pacer {pub interval:Duration,last:Option<Instant>,last_query:Instant,#[cfg(windows)] hwnd:isize}
impl Default for Pacer {fn default()->Self{Self{interval:interval(60000),last:None,last_query:Instant::now()-Duration::from_secs(3),#[cfg(windows)]hwnd:0}}}
impl Pacer {
    pub fn set_window(&mut self,window:&impl raw_window_handle::HasWindowHandle){
        #[cfg(windows)] {if let Ok(raw)=window.window_handle(){if let raw_window_handle::RawWindowHandle::Win32(win)=raw.as_raw(){self.hwnd=win.hwnd.get();self.refresh();}}}
        #[cfg(not(windows))]let _=window;
    }
    pub fn refresh(&mut self){
        #[cfg(windows)] {self.interval=interval(windows_refresh(self.hwnd));}
        self.last_query=Instant::now();
        if std::env::var("FEATHER_UI_PREVIEW").ok().as_deref()==Some("1"){
            if let Ok(path)=std::env::var("FEATHER_GUI_PACING_REPORT"){
                if let Some(parent)=std::path::Path::new(&path).parent(){let _=std::fs::create_dir_all(parent);}
                let target=1.0/self.interval.as_secs_f64();let _=std::fs::write(path,format!("monitor_refresh_hz={:.3}\nmax_fps={target:.3}\nmin_interval_ms={:.3}\n",target+1.0,self.interval.as_secs_f64()*1000.0));
            }
        }
    }
    /// Called once before egui starts a native frame (not per layout pass).
    /// Sleep is bounded to <=one monitor interval and occurs before submitting
    /// GPU work; no spinning, no system setting change. Event timers remain lazy.
    pub fn before_frame(&mut self){
        if self.last_query.elapsed()>Duration::from_secs(2){self.refresh();}
        if let Some(last)=self.last{let wait=(last+self.interval).saturating_duration_since(Instant::now());if !wait.is_zero(){std::thread::sleep(wait);}}
        self.last=Some(Instant::now());
    }
}
#[cfg(windows)]fn windows_refresh(hwnd:isize)->u32 {
    use windows_sys::Win32::Graphics::Gdi::{MonitorFromWindow,GetMonitorInfoW,EnumDisplaySettingsW,MONITORINFOEXW,DEVMODEW,MONITOR_DEFAULTTONEAREST,ENUM_CURRENT_SETTINGS};
    // SAFETY: zero-initialized, sized native structs; HWND is borrowed from live
    // GUI; APIs only read current display mode. No global state is mutated.
    unsafe {
        let monitor=MonitorFromWindow(hwnd as _,MONITOR_DEFAULTTONEAREST);let mut info:MONITORINFOEXW=std::mem::zeroed();info.monitorInfo.cbSize=std::mem::size_of::<MONITORINFOEXW>() as u32;
        let mut mode:DEVMODEW=std::mem::zeroed();mode.dmSize=std::mem::size_of::<DEVMODEW>() as u16;
        if GetMonitorInfoW(monitor,&mut info.monitorInfo)!=0&&EnumDisplaySettingsW(info.szDevice.as_ptr(),ENUM_CURRENT_SETTINGS,&mut mode)!=0&&mode.dmDisplayFrequency>1 {return mode.dmDisplayFrequency.saturating_mul(1000);}
    }60000
}
#[cfg(test)]mod tests{use super::*;#[test]fn monitor_minus_one_is_a_maximum_not_an_idle_loop(){assert!((interval(165000).as_secs_f64()-1.0/164.0).abs()<1e-8);assert!((interval(60000).as_secs_f64()-1.0/59.0).abs()<1e-8);assert_eq!(interval(0),interval(60000));}}
