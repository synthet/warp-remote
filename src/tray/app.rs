use std::cell::OnceCell;
use std::path::PathBuf;
use std::ptr;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use clap::Parser;
use native_windows_gui as nwg;
use remote_control_server::launch::{self, Args};
use remote_control_server::tray_support::{LogTail, dashboard_url, load_or_create_token};
use winapi::shared::winerror::ERROR_ALREADY_EXISTS;
use winapi::um::errhandlingapi::GetLastError;
use winapi::um::shellapi::ShellExecuteW;
use winapi::um::synchapi::CreateMutexW;
use winapi::um::winuser::{
    FindWindowW, SW_RESTORE, SW_SHOWNORMAL, SetForegroundWindow, ShowWindow,
};

const TITLE: &str = "Synth Remote Control";
const WINDOW_CLASS: &str = "NativeWindowsGuiWindow";
const INSTANCE_MUTEX: &str = "Local\\SynthRemoteControlTray";
/// The icon's id in `assets/app.rc`.
const ICON_RESOURCE_ID: usize = 1;

enum Status {
    Starting,
    Running,
    Failed(String),
}

#[derive(Default)]
struct App {
    url: String,
    token: String,
    status: Arc<Mutex<Option<Status>>>,
    log: OnceCell<LogTail>,

    icon: nwg::Icon,
    window: nwg::Window,
    status_label: nwg::Label,
    open_button: nwg::Button,
    copy_url_button: nwg::Button,
    copy_token_button: nwg::Button,
    log_box: nwg::TextBox,
    notice: nwg::Notice,
    tray: nwg::TrayNotification,
    menu: nwg::Menu,
    menu_show: nwg::MenuItem,
    menu_open: nwg::MenuItem,
    menu_copy_token: nwg::MenuItem,
    menu_separator: nwg::MenuSeparator,
    menu_quit: nwg::MenuItem,
}

impl App {
    fn build(&mut self) -> Result<(), nwg::NwgError> {
        let resources = nwg::EmbedResource::load(None)?;
        nwg::Icon::builder()
            .source_embed(Some(&resources))
            .source_embed_id(ICON_RESOURCE_ID)
            .build(&mut self.icon)?;
        nwg::Window::builder()
            .flags(nwg::WindowFlags::WINDOW | nwg::WindowFlags::MINIMIZE_BOX)
            .size((640, 420))
            .center(true)
            .title(TITLE)
            .icon(Some(&self.icon))
            .build(&mut self.window)?;
        nwg::Label::builder()
            .parent(&self.window)
            .position((12, 12))
            .size((616, 24))
            .text("Starting...")
            .build(&mut self.status_label)?;
        for (button, text, x) in [
            (&mut self.open_button, "Open dashboard", 12),
            (&mut self.copy_url_button, "Copy URL", 172),
            (&mut self.copy_token_button, "Copy token", 332),
        ] {
            nwg::Button::builder()
                .parent(&self.window)
                .position((x, 44))
                .size((150, 30))
                .text(text)
                .build(button)?;
        }
        nwg::TextBox::builder()
            .parent(&self.window)
            .position((12, 84))
            .size((616, 324))
            .readonly(true)
            .flags(
                nwg::TextBoxFlags::VISIBLE
                    | nwg::TextBoxFlags::VSCROLL
                    | nwg::TextBoxFlags::AUTOVSCROLL,
            )
            .build(&mut self.log_box)?;
        nwg::Notice::builder()
            .parent(&self.window)
            .build(&mut self.notice)?;
        nwg::TrayNotification::builder()
            .parent(&self.window)
            .icon(Some(&self.icon))
            .tip(Some(TITLE))
            .build(&mut self.tray)?;
        nwg::Menu::builder()
            .popup(true)
            .parent(&self.window)
            .build(&mut self.menu)?;
        for (item, text) in [
            (&mut self.menu_show, "Show window"),
            (&mut self.menu_open, "Open dashboard"),
            (&mut self.menu_copy_token, "Copy token"),
        ] {
            nwg::MenuItem::builder()
                .parent(&self.menu)
                .text(text)
                .build(item)?;
        }
        nwg::MenuSeparator::builder()
            .parent(&self.menu)
            .build(&mut self.menu_separator)?;
        nwg::MenuItem::builder()
            .parent(&self.menu)
            .text("Quit")
            .build(&mut self.menu_quit)
    }

    fn collapse(&self) {
        self.window.set_visible(false);
    }

    fn reveal(&self) {
        self.window.restore();
        self.window.set_visible(true);
        self.refresh();
        if let Some(hwnd) = self.window.handle.hwnd() {
            // SAFETY: `hwnd` is this app's live window.
            unsafe { SetForegroundWindow(hwnd) };
        }
    }

    fn toggle(&self) {
        if self.window.visible() {
            self.collapse();
        } else {
            self.reveal();
        }
    }

    /// Shows the latest status and log. Runs on the UI thread.
    fn refresh(&self) {
        let (text, failed) = match self.status.lock().expect("status lock").as_ref() {
            Some(Status::Running) => (format!("Running at {}", self.url), false),
            Some(Status::Failed(error)) => (format!("Stopped: {error}"), true),
            Some(Status::Starting) | None => ("Starting...".to_owned(), false),
        };
        self.status_label.set_text(&text);
        self.tray.set_tip(&format!("{TITLE}\n{text}"));
        if self.window.visible()
            && let Some(log) = self.log.get()
        {
            self.log_box.set_text(&log.text());
            self.log_box.scroll_lastline();
        }
        // A failed server is otherwise invisible while the window is collapsed.
        if failed && !self.window.visible() {
            self.reveal();
        }
    }

    fn copy(&self, what: &str, text: &str) {
        nwg::Clipboard::set_data_text(&self.window, text);
        log::info!("Copied the {what} to the clipboard");
    }

    fn open_dashboard(&self) {
        let operation = wide("open");
        let url = wide(&self.url);
        // SAFETY: both strings are nul-terminated and outlive the call.
        unsafe {
            ShellExecuteW(
                ptr::null_mut(),
                operation.as_ptr(),
                url.as_ptr(),
                ptr::null(),
                ptr::null(),
                SW_SHOWNORMAL,
            );
        }
    }

    fn handle_event(&self, event: nwg::Event, data: &nwg::EventData, handle: nwg::ControlHandle) {
        use nwg::Event as E;

        match event {
            E::OnWindowClose if handle == self.window.handle => {
                if let nwg::EventData::OnWindowClose(close) = data {
                    close.close(false);
                }
                self.collapse();
            }
            E::OnWindowMinimize if handle == self.window.handle => self.collapse(),
            E::OnNotice if handle == self.notice.handle => self.refresh(),
            E::OnMousePress(nwg::MousePressEvent::MousePressLeftUp)
                if handle == self.tray.handle =>
            {
                self.toggle();
            }
            E::OnContextMenu if handle == self.tray.handle => {
                let (x, y) = nwg::GlobalCursor::position();
                self.menu.popup(x, y);
            }
            E::OnButtonClick if handle == self.open_button.handle => self.open_dashboard(),
            E::OnButtonClick if handle == self.copy_url_button.handle => {
                self.copy("URL", &self.url);
            }
            E::OnButtonClick if handle == self.copy_token_button.handle => {
                self.copy("token", &self.token);
            }
            E::OnMenuItemSelected if handle == self.menu_show.handle => self.reveal(),
            E::OnMenuItemSelected if handle == self.menu_open.handle => self.open_dashboard(),
            E::OnMenuItemSelected if handle == self.menu_copy_token.handle => {
                self.copy("token", &self.token);
            }
            E::OnMenuItemSelected if handle == self.menu_quit.handle => {
                nwg::stop_thread_dispatch();
            }
            _ => {}
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Claims the single-instance mutex, which stays held until the process exits.
fn is_first_instance() -> bool {
    let name = wide(INSTANCE_MUTEX);
    // SAFETY: `name` is nul-terminated and outlives the call.
    let (mutex, error) = unsafe {
        let mutex = CreateMutexW(ptr::null_mut(), 0, name.as_ptr());
        (mutex, GetLastError())
    };
    mutex.is_null() || error != ERROR_ALREADY_EXISTS
}

/// Brings up the window of the instance that is already running, even if it is collapsed.
fn reveal_running_instance() {
    let class = wide(WINDOW_CLASS);
    let title = wide(TITLE);
    // SAFETY: both strings are nul-terminated and outlive the call, and null windows are skipped.
    unsafe {
        let window = FindWindowW(class.as_ptr(), title.as_ptr());
        if !window.is_null() {
            ShowWindow(window, SW_RESTORE);
            SetForegroundWindow(window);
        }
    }
}

fn data_dir() -> anyhow::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(base).join("SynthRemoteControl"))
}

/// Runs the server on its own thread. The thread ends with the process, so Quit needs no shutdown
/// step: sessions live in memory and terminals reconnect on their own.
fn spawn_server(
    args: Args,
    token: String,
    status: Arc<Mutex<Option<Status>>>,
    notice: nwg::NoticeSender,
) {
    std::thread::spawn(move || {
        let set_status = |new: Status| {
            *status.lock().expect("status lock") = Some(new);
            notice.notice();
        };
        let result = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("starting the async runtime")
            .and_then(|runtime| {
                runtime.block_on(async {
                    let listener = launch::bind(&args).await?;
                    set_status(Status::Running);
                    launch::serve(listener, &args, token, std::future::pending()).await
                })
            });
        if let Err(error) = result {
            log::error!("{error:#}");
            set_status(Status::Failed(format!("{error:#}")));
        }
    });
}

fn start() -> anyhow::Result<()> {
    let args = Args::try_parse().map_err(|e| anyhow::anyhow!("{e}"))?;
    let dir = data_dir()?;
    let token = match args.configured_token()? {
        Some(token) => token,
        None => load_or_create_token(&dir.join("token"))?,
    };

    nwg::init().context("starting the Windows UI")?;
    nwg::Font::set_global_family("Segoe UI").context("setting the UI font")?;

    let mut app = App {
        url: dashboard_url(args.bind),
        token: token.clone(),
        status: Arc::new(Mutex::new(Some(Status::Starting))),
        ..App::default()
    };
    app.build().context("building the window")?;
    let app = Rc::new(app);

    let notice = app.notice.sender();
    let log = LogTail::create(&dir.join("server.log"), move || notice.notice())?;
    let _ = app.log.set(log.clone());
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .write_style(env_logger::WriteStyle::Never)
        .target(env_logger::Target::Pipe(Box::new(log)))
        .init();

    let weak = Rc::downgrade(&app);
    let handler = nwg::full_bind_event_handler(&app.window.handle, move |event, data, handle| {
        if let Some(app) = weak.upgrade() {
            app.handle_event(event, &data, handle);
        }
    });

    app.window.set_visible(true);
    spawn_server(args, token, app.status.clone(), app.notice.sender());
    nwg::dispatch_thread_events();

    nwg::unbind_event_handler(&handler);
    Ok(())
}

pub fn run() {
    if !is_first_instance() {
        reveal_running_instance();
        return;
    }
    if let Err(error) = start() {
        nwg::error_message(TITLE, &format!("{error:#}"));
    }
}
