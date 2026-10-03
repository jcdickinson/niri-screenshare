use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk4::gdk::Display;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, CssProvider, Label, ListBox, Orientation,
    PolicyType, ScrolledWindow, SelectionMode, STYLE_PROVIDER_PRIORITY_APPLICATION,
};
#[allow(unused_imports)]
use libadwaita::prelude::*;
use libadwaita::{ActionRow, HeaderBar, ViewStack, ViewSwitcher, ViewSwitcherPolicy};

use super::{DisplayItem, PickerChoice, WindowItem};

const PICKER_APP_ID: &str = "io.github.niri.screenshare.picker";

pub(super) fn run(
    displays: Vec<DisplayItem>,
    windows: Vec<WindowItem>,
) -> anyhow::Result<Option<PickerChoice>> {
    let result: Rc<RefCell<Option<PickerChoice>>> = Rc::new(RefCell::new(None));

    let app = Application::builder()
        .application_id(PICKER_APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    let result_activate = result.clone();
    app.connect_activate(move |app| {
        build_and_present(
            app,
            displays.clone(),
            windows.clone(),
            result_activate.clone(),
        );
    });

    let exit = app.run_with_args::<&str>(&[]);
    if exit != glib::ExitCode::SUCCESS {
        tracing::warn!("picker gtk application exited with {exit:?}");
    }

    let choice = result.borrow().clone();
    Ok(choice)
}

fn build_and_present(
    app: &Application,
    displays: Vec<DisplayItem>,
    windows: Vec<WindowItem>,
    result: Rc<RefCell<Option<PickerChoice>>>,
) {
    let _ = libadwaita::init();

    let selection: Rc<RefCell<Option<PickerChoice>>> = Rc::new(RefCell::new(None));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Screen Sharing")
        .default_width(520)
        .default_height(480)
        .build();

    let header = HeaderBar::new();

    let stack = ViewStack::new();
    stack.set_vexpand(true);

    let switcher = ViewSwitcher::builder()
        .stack(&stack)
        .policy(ViewSwitcherPolicy::Wide)
        .build();
    header.set_title_widget(Some(&switcher));

    let display_list = build_display_list(&displays, &selection);
    let display_scroll = ScrolledWindow::builder()
        .child(&display_list)
        .hscrollbar_policy(PolicyType::Never)
        .vexpand(true)
        .build();
    let displays_page = stack.add_titled(&display_scroll, Some("displays"), "Displays");
    displays_page.set_icon_name(Some("video-display-symbolic"));

    let window_list = build_window_list(&windows, &selection);
    let window_scroll = ScrolledWindow::builder()
        .child(&window_list)
        .hscrollbar_policy(PolicyType::Never)
        .vexpand(true)
        .build();
    let windows_page = stack.add_titled(&window_scroll, Some("windows"), "Windows");
    windows_page.set_icon_name(Some("window-symbolic"));

    stack.set_visible_child_name(initial_page(!displays.is_empty(), !windows.is_empty()));

    let cancel = gtk4::Button::with_label("Cancel");
    cancel.add_css_class("pill");
    let share = gtk4::Button::with_label("Share");
    share.add_css_class("pill");
    share.add_css_class("suggested-action");
    share.set_sensitive(false);

    let share_btn = share.clone();
    let selection_watch = selection.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        share_btn.set_sensitive(selection_watch.borrow().is_some());
        glib::ControlFlow::Continue
    });

    let button_row = GtkBox::new(Orientation::Horizontal, 12);
    button_row.set_halign(Align::End);
    button_row.set_margin_top(6);
    button_row.set_margin_bottom(12);
    button_row.set_margin_start(12);
    button_row.set_margin_end(12);
    button_row.append(&cancel);
    button_row.append(&share);

    let content = GtkBox::new(Orientation::Vertical, 0);
    content.append(&header);
    content.append(&stack);
    content.append(&button_row);
    window.set_child(Some(&content));

    let finish = Rc::new({
        let result = result.clone();
        let selection = selection.clone();
        let window = window.clone();
        move |accepted: bool| {
            if accepted {
                *result.borrow_mut() = selection.borrow().clone();
            }
            window.close();
        }
    });

    cancel.connect_clicked({
        let finish = finish.clone();
        move |_| finish(false)
    });
    share.connect_clicked({
        let finish = finish.clone();
        let selection = selection.clone();
        move |_| {
            if selection.borrow().is_some() {
                finish(true);
            }
        }
    });

    display_list.connect_row_activated({
        let finish = finish.clone();
        let selection = selection.clone();
        move |_, _| {
            if selection.borrow().is_some() {
                finish(true);
            }
        }
    });
    window_list.connect_row_activated({
        let finish = finish.clone();
        let selection = selection.clone();
        move |_, _| {
            if selection.borrow().is_some() {
                finish(true);
            }
        }
    });

    if let Some(display) = Display::default() {
        let provider = CssProvider::new();
        provider.load_from_data("headerbar { min-height: 48px; }");
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    // niri floats fixed-size windows at map time, so opening non-resizable makes
    // the picker come up floating instead of being tiled and moved afterwards.
    // Resizability is handed back once it is mapped; the floating state sticks.
    window.set_resizable(false);
    window.connect_map(|window| {
        let window = window.clone();
        glib::timeout_add_local_once(Duration::from_millis(150), move || {
            window.set_resizable(true);
        });
    });
    window.present();
}

/// Opens the picker on a tab with available targets. SelectSources filters
/// the lists to the requested source types, so an empty side is either
/// unrequested or has no currently available targets.
fn initial_page(has_displays: bool, has_windows: bool) -> &'static str {
    if !has_displays && has_windows {
        "windows"
    } else {
        "displays"
    }
}

fn build_display_list(
    displays: &[DisplayItem],
    selection: &Rc<RefCell<Option<PickerChoice>>>,
) -> ListBox {
    let list = ListBox::new();
    list.set_selection_mode(SelectionMode::Single);
    list.add_css_class("boxed-list");
    list.set_margin_top(12);
    list.set_margin_bottom(12);
    list.set_margin_start(12);
    list.set_margin_end(12);

    if displays.is_empty() {
        list.append(&empty_label("No displays found"));
        return list;
    }

    for d in displays {
        let row = ActionRow::builder()
            .title(&d.name)
            .subtitle(format!("{}×{}", d.width, d.height))
            .activatable(true)
            .selectable(true)
            .build();
        list.append(&row);
    }

    let items = displays.to_vec();
    let selection = selection.clone();
    list.connect_row_selected(move |_, row| {
        let Some(row) = row else { return };
        if let Some(d) = items.get(row.index() as usize) {
            *selection.borrow_mut() = Some(PickerChoice::Monitor(d.name.clone()));
        }
    });

    list
}

fn build_window_list(
    windows: &[WindowItem],
    selection: &Rc<RefCell<Option<PickerChoice>>>,
) -> ListBox {
    let list = ListBox::new();
    list.set_selection_mode(SelectionMode::Single);
    list.add_css_class("boxed-list");
    list.set_margin_top(12);
    list.set_margin_bottom(12);
    list.set_margin_start(12);
    list.set_margin_end(12);

    if windows.is_empty() {
        list.append(&empty_label("No windows found"));
        return list;
    }

    for w in windows {
        let title = if w.title.is_empty() {
            w.app_id.clone()
        } else {
            w.title.clone()
        };
        let subtitle = if w.app_id.is_empty() || w.app_id == title {
            format!("{}×{}", w.width, w.height)
        } else {
            format!("{} · {}×{}", w.app_id, w.width, w.height)
        };
        let row = ActionRow::builder()
            .title(&title)
            .subtitle(subtitle)
            .activatable(true)
            .selectable(true)
            .build();
        list.append(&row);
    }

    let items = windows.to_vec();
    let selection = selection.clone();
    list.connect_row_selected(move |_, row| {
        let Some(row) = row else { return };
        if let Some(w) = items.get(row.index() as usize) {
            *selection.borrow_mut() = Some(PickerChoice::Window(w.id));
        }
    });

    list
}

fn empty_label(text: &str) -> Label {
    let label = Label::new(Some(text));
    label.add_css_class("dim-label");
    label.set_margin_top(24);
    label.set_margin_bottom(24);
    label.set_halign(Align::Center);
    label
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initial_page_prefers_windows_when_no_displays() {
        assert_eq!(initial_page(false, true), "windows");
    }
    #[test]
    fn initial_page_stays_on_displays_when_present() {
        assert_eq!(initial_page(true, true), "displays");
        assert_eq!(initial_page(true, false), "displays");
    }
    #[test]
    fn initial_page_falls_back_to_displays_when_empty() {
        assert_eq!(initial_page(false, false), "displays");
    }
}
