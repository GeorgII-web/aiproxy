//! Windows tray icon and menu. Runs the OS event loop on the main thread.

use std::sync::Arc;

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tokio::runtime::Runtime;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

use crate::app::AppState;
use crate::autostart;
use crate::config::APP_NAME;

enum UserEvent {
    Menu(MenuEvent),
    Tray,
}

pub fn run(state: Arc<AppState>, rt: Runtime) -> ! {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::Menu(e));
    }));
    let proxy = event_loop.create_proxy();
    TrayIconEvent::set_event_handler(Some(move |_| {
        let _ = proxy.send_event(UserEvent::Tray);
    }));

    let status = MenuItem::new(status_text(&state), false, None);
    let open_settings = MenuItem::new("Open settings", true, None);
    let open_logs = MenuItem::new("Open logs folder", true, None);
    let restart = MenuItem::new("Restart server", true, None);
    let autostart_item =
        CheckMenuItem::new("Start with Windows", true, autostart::is_enabled(), None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append_items(&[
        &status,
        &PredefinedMenuItem::separator(),
        &open_settings,
        &open_logs,
        &restart,
        &autostart_item,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .expect("menu");

    let mut tray = None;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                tray = Some(
                    TrayIconBuilder::new()
                        .with_menu(Box::new(menu.clone()))
                        .with_tooltip(format!("{APP_NAME} · {}/v1", state.base_url()))
                        .with_icon(icon())
                        .build()
                        .expect("tray icon"),
                );
            }
            Event::UserEvent(UserEvent::Tray) => {
                status.set_text(status_text(&state));
                autostart_item.set_checked(autostart::is_enabled());
                if let Some(t) = &tray {
                    let _ = t.set_tooltip(Some(format!("{APP_NAME} · {}/v1", state.base_url())));
                }
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                let id = &e.id;
                if id == open_settings.id() {
                    open(state.base_url());
                } else if id == open_logs.id() {
                    open(state.paths.logs_dir.display().to_string());
                } else if id == restart.id() {
                    let s = state.clone();
                    rt.spawn(async move {
                        if let Err(e) = s.start_server().await {
                            tracing::error!("{e:#}");
                        }
                    });
                } else if id == autostart_item.id() {
                    if let Err(e) = state.set_autostart(autostart_item.is_checked()) {
                        tracing::error!("{e:#}");
                    }
                    autostart_item.set_checked(autostart::is_enabled());
                } else if id == quit.id() {
                    rt.block_on(state.shutdown());
                    tray.take();
                    std::process::exit(0);
                }
            }
            _ => {}
        }
    })
}

fn status_text(state: &AppState) -> String {
    format!(
        "{APP_NAME} {} · {}/v1",
        env!("CARGO_PKG_VERSION"),
        state.base_url()
    )
}

fn open(target: String) {
    if let Err(e) = open::that_detached(&target) {
        tracing::warn!("cannot open {target}: {e}");
    }
}

/// A blue disc with a white ring, drawn in code so no asset file is needed.
fn icon() -> Icon {
    const N: usize = 32;
    let mut rgba = Vec::with_capacity(N * N * 4);
    let c = (N as f32 - 1.0) / 2.0;
    for y in 0..N {
        for x in 0..N {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let px: [u8; 4] = if d > 15.0 {
                [0, 0, 0, 0]
            } else if d > 12.0 {
                [255, 255, 255, 255]
            } else if d > 9.5 {
                [37, 99, 235, 255]
            } else if d > 7.0 {
                [255, 255, 255, 255]
            } else {
                [37, 99, 235, 255]
            };
            rgba.extend_from_slice(&px);
        }
    }
    Icon::from_rgba(rgba, N as u32, N as u32).expect("icon")
}
