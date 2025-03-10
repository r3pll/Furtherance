// Furtherance - Track your time without being tracked
// Copyright (C) 2022  Ricky Kresslein <rk@lakoliu.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use adw::subclass::prelude::AdwApplicationWindowImpl;
use gettextrs::*;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib, CompositeTemplate};
use glib::{clone, timeout_add_local};
use std::time::Duration;
use std::sync::Mutex;
use std::rc::Rc;
use std::cell::RefCell;
use chrono::{DateTime, Local, NaiveDateTime, ParseError, Duration as ChronDur, offset::TimeZone};
use dbus::blocking::Connection;
use itertools::Itertools;
use std::fs::{File, create_dir_all, remove_file};
use std::io::{self, BufWriter, Write, BufReader, BufRead};
use directories::ProjectDirs;
use std::path::PathBuf;

use crate::ui::FurHistoryBox;
use crate::FurtheranceApplication;
use crate::database;
use crate::settings_manager;
use crate::config;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/com/lakoliu/Furtherance/gtk/window.ui")]
    pub struct FurtheranceWindow {
        // Template widgets
        #[template_child]
        pub header_bar: TemplateChild<adw::HeaderBar>,
        #[template_child]
        pub add_task: TemplateChild<gtk::Button>,
        #[template_child]
        pub watch: TemplateChild<gtk::Label>,
        #[template_child]
        pub task_input: TemplateChild<gtk::Entry>,
        #[template_child]
        pub start_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub history_box: TemplateChild<FurHistoryBox>,
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,

        pub stored_idle: Mutex<u64>,
        pub idle_notified: Mutex<bool>,
        pub idle_time_reached: Mutex<bool>,
        pub subtract_idle: Mutex<bool>,
        pub idle_start_time: Mutex<String>,
        pub running: Mutex<bool>,
        pub pomodoro_continue: Mutex<bool>,
        pub idle_dialog: Mutex<gtk::MessageDialog>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FurtheranceWindow {
        const NAME: &'static str = "FurtheranceWindow";
        type Type = super::FurtheranceWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            FurHistoryBox::static_type();
            Self::bind_template(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for FurtheranceWindow {
        fn constructed(&self, obj: &Self::Type) {
            obj.setup_widgets();
            obj.setup_signals();
            obj.setup_settings();
            self.parent_constructed(obj);
        }
    }
    impl WidgetImpl for FurtheranceWindow {}
    impl WindowImpl for FurtheranceWindow {}
    impl ApplicationWindowImpl for FurtheranceWindow {}
    impl AdwApplicationWindowImpl for FurtheranceWindow {}
}

glib::wrapper! {
    pub struct FurtheranceWindow(ObjectSubclass<imp::FurtheranceWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl FurtheranceWindow {
    pub fn new<P: glib::IsA<gtk::Application>>(application: &P) -> Self {
        glib::Object::new(&[("application", application)])
            .expect("Failed to create FurtheranceWindow")
    }

    pub fn display_toast(&self, text: &str) {
        // Display in-app notifications
        let imp = imp::FurtheranceWindow::from_instance(self);
        let toast = adw::Toast::new(text);
        imp.toast_overlay.add_toast(&toast);
    }

    fn set_watch_time(&self, text: &str) {
        // Update watch time while timer is running
        let imp = imp::FurtheranceWindow::from_instance(self);
        imp.watch.set_text(text);
        if settings_manager::get_bool("notify-of-idle") {
            self.check_user_idle();
        }
    }

    pub fn save_task(&self, start_time: DateTime<Local>, mut stop_time: DateTime<Local>) {
        // Save the most recent task to the database and clear the task_input field
        let imp = imp::FurtheranceWindow::from_instance(self);

        if *imp.subtract_idle.lock().unwrap() {
            let idle_start = DateTime::parse_from_rfc3339(&imp.idle_start_time.lock().unwrap()).unwrap();
            stop_time = idle_start.with_timezone(&Local);
            *imp.subtract_idle.lock().unwrap() = false;
        }

        let (task_name, tag_list) = self.split_tags_and_task();
        let _ = database::db_write(&task_name, start_time, stop_time, tag_list);
        imp.task_input.set_text("");
        imp.history_box.create_tasks_page();
        self.reset_idle();
    }

    pub fn reset_history_box(&self) {
        let imp = imp::FurtheranceWindow::from_instance(self);
        imp.history_box.create_tasks_page();
    }

    fn setup_widgets(&self) {
        let imp = imp::FurtheranceWindow::from_instance(self);

        // Set initial minimum height
        let is_saved_task: bool = match database::check_for_tasks() {
            Ok(_) => true,
            Err(_) => false,
        };
        if is_saved_task {
            self.set_height_request(300);
        } else {
            self.set_height_request(390);
        }

        // Development mode
        if config::PROFILE == "development" {
            self.add_css_class("devel");
        }

        *imp.pomodoro_continue.lock().unwrap() = false;
        imp.start_button.set_sensitive(false);
        imp.start_button.add_css_class("suggested-action");
        self.refresh_timer();
        imp.task_input.grab_focus();

        if settings_manager::get_bool("autosave") {
            self.check_for_autosave();
        }
    }

    fn setup_signals(&self) {
        let imp = imp::FurtheranceWindow::from_instance(self);
        *imp.running.lock().unwrap() = false;
        let start_time = Rc::new(RefCell::new(Local::now()));
        let stop_time = Rc::new(RefCell::new(Local::now()));

        imp.task_input.connect_changed(clone!(@weak self as this => move |task_input| {
            let imp2 = imp::FurtheranceWindow::from_instance(&this);
            let task_input_text = task_input.text();
            let split_tags: Vec<&str> = task_input_text.trim().split("#").collect();
            if split_tags[0].trim().is_empty() {
                imp2.start_button.set_sensitive(false);
            } else {
                imp2.start_button.set_sensitive(true);
            }
        }));

        imp.start_button.connect_clicked(clone!(@weak self as this => move |button| {
            let imp2 = imp::FurtheranceWindow::from_instance(&this);
            if !*imp2.running.lock().unwrap() {
                if settings_manager::get_bool("pomodoro") && !*imp2.pomodoro_continue.lock().unwrap() {
                    let pomodoro_time = settings_manager::get_int("pomodoro-time");
                    let mut secs: i32 = 0;
                    let mut mins: i32 = pomodoro_time;
                    let mut hrs: i32 = mins / 60;
                    mins = mins % 60;

                    *imp2.running.lock().unwrap() = true;
                    *start_time.borrow_mut() = Local::now();
                    let timer_start = *start_time.borrow();
                    imp2.task_input.set_sensitive(false);
                    let duration = Duration::new(1,0);
                    timeout_add_local(duration, clone!(@strong this as this_clone => move || {
                        let imp3 = imp::FurtheranceWindow::from_instance(&this_clone);
                        if *imp3.running.lock().unwrap() {
                            secs -= 1;
                            if secs < 0 {
                                secs = 59;
                                mins -= 1;
                                if mins < 0 {
                                    mins = 59;
                                    hrs -= 1;
                                }
                            }
                            let watch_text: &str = &format!("{:02}:{:02}:{:02}", hrs, mins, secs).to_string();
                            this_clone.set_watch_time(watch_text);
                        }
                        if settings_manager::get_bool("autosave") {
                            let autosave_mins = settings_manager::get_int("autosave-time");
                            let total_elapsed = (pomodoro_time * 60) - (hrs * 3600) - (mins * 60) - secs;
                            if total_elapsed % (autosave_mins * 60) == 0 {
                                this_clone.write_autosave(timer_start);
                            }
                        }
                        if hrs == 0 && mins == 0 && secs == 0 {
                            let timer_stop = Local::now();
                            *imp3.running.lock().unwrap() = false;
                            this_clone.pomodoro_over(timer_start, timer_stop);
                        }
                        Continue(*imp3.running.lock().unwrap())
                    }));
                } else {
                    let mut secs: u32 = 0;
                    let mut mins: u32 = 0;
                    let mut hrs: u32 = 0;

                    if *imp2.pomodoro_continue.lock().unwrap() {
                        let pomodoro_start_time = *start_time.borrow();
                        let now_time = Local::now();
                        let continue_time = now_time - pomodoro_start_time;
                        let continue_time = continue_time.num_seconds() as u32;
                        hrs = continue_time / 3600;
                        mins = continue_time % 3600 / 60;
                        secs = continue_time % 60;
                        let watch_text: &str = &format!("{:02}:{:02}:{:02}", hrs, mins, secs).to_string();
                        this.set_watch_time(watch_text);

                        *imp2.pomodoro_continue.lock().unwrap() = false;
                    } else {
                        *start_time.borrow_mut() = Local::now();
                    }

                    *imp2.running.lock().unwrap() = true;
                    imp2.task_input.set_sensitive(false);
                    let autosave_start = *start_time.borrow();
                    let duration = Duration::new(1,0);
                    timeout_add_local(duration, clone!(@strong this as this_clone => move || {
                        let imp3 = imp::FurtheranceWindow::from_instance(&this_clone);
                        if *imp3.running.lock().unwrap() {
                            secs += 1;
                            if secs > 59 {
                                secs = 0;
                                mins += 1;
                                if mins > 59 {
                                    mins = 0;
                                    hrs += 1;
                                }
                            }
                            let watch_text: &str = &format!("{:02}:{:02}:{:02}", hrs, mins, secs).to_string();
                            this_clone.set_watch_time(watch_text);

                            if settings_manager::get_bool("autosave") {
                                let autosave_mins = settings_manager::get_int("autosave-time") as u32;
                                let total_elapsed = (hrs * 3600) + (mins * 60) + secs;
                                if total_elapsed % (autosave_mins * 60) == 0 {
                                    this_clone.write_autosave(autosave_start);
                                }
                            }
                        }
                        Continue(*imp3.running.lock().unwrap())
                    }));
                }
                button.set_icon_name("media-playback-stop-symbolic");
            } else {
                *stop_time.borrow_mut() = Local::now();
                *imp2.running.lock().unwrap() = false;
                button.set_icon_name("media-playback-start-symbolic");
                this.refresh_timer();
                imp2.task_input.set_sensitive(true);
                this.save_task(*start_time.borrow(), *stop_time.borrow());
                FurtheranceWindow::delete_autosave();
            }
        }));

        imp.add_task.connect_clicked(clone!(@weak self as this => move |_| {
            let dialog = gtk::MessageDialog::new(
                Some(&this),
                gtk::DialogFlags::MODAL,
                gtk::MessageType::Question,
                gtk::ButtonsType::None,
                &format!("<span size='x-large' weight='bold'>{}</span>", &gettext("New Task")),
            );
            dialog.set_use_markup(true);
            dialog.add_buttons(&[
                (&gettext("Cancel"), gtk::ResponseType::Cancel),
                (&gettext("Add"), gtk::ResponseType::Ok)
            ]);

            let message_area = dialog.message_area().downcast::<gtk::Box>().unwrap();
            let vert_box = gtk::Box::new(gtk::Orientation::Vertical, 5);
            let task_name_edit = gtk::Entry::new();
            task_name_edit.set_placeholder_text(Some(&gettext("Task Name")));
            let task_tags_edit = gtk::Entry::new();
            let tags_placeholder = format!("#{}", &gettext("tags"));
            task_tags_edit.set_placeholder_text(Some(&tags_placeholder));

            let labels_box = gtk::Box::new(gtk::Orientation::Horizontal, 5);
            labels_box.set_homogeneous(true);
            let start_label = gtk::Label::new(Some(&gettext("Start")));
            start_label.add_css_class("title-4");
            let stop_label = gtk::Label::new(Some(&gettext("Stop")));
            stop_label.add_css_class("title-4");
            let times_box = gtk::Box::new(gtk::Orientation::Horizontal, 5);
            times_box.set_homogeneous(true);

            let stop_time = Local::now();
            let start_time = stop_time - ChronDur::seconds(1);

            let mut start_time_w_year = start_time.format("%h %d %Y %H:%M:%S").to_string();
            if !settings_manager::get_bool("show-seconds") {
                start_time_w_year = start_time.format("%h %d %Y %H:%M").to_string();
            }
            let mut stop_time_w_year = stop_time.format("%h %d %Y %H:%M:%S").to_string();
            if !settings_manager::get_bool("show-seconds") {
                stop_time_w_year = stop_time.format("%h %d %Y %H:%M").to_string();
            }
            let start_time_edit = gtk::Entry::new();
            start_time_edit.set_text(&start_time_w_year);
            let stop_time_edit = gtk::Entry::new();
            stop_time_edit.set_text(&stop_time_w_year);

            let instructions = gtk::Label::new(Some(
                &gettext("*Use the format MMM DD YYYY HH:MM:SS")));
            if !settings_manager::get_bool("show-seconds") {
                instructions.set_text(&gettext("*Use the format MMM DD YYYY HH:MM"));
            }
            instructions.set_visible(false);
            instructions.add_css_class("error_message");

            let time_error = gtk::Label::new(Some(
                &gettext("*Start time cannot be later than stop time.")));
            time_error.set_visible(false);
            time_error.add_css_class("error_message");

            let future_error = gtk::Label::new(Some(
                &gettext("*Time cannot be in the future.")));
            future_error.set_visible(false);
            future_error.add_css_class("error_message");

            let name_error = gtk::Label::new(Some(
                &gettext("*Task name cannot be blank.")));
            name_error.set_visible(false);
            name_error.add_css_class("error_message");

            vert_box.append(&task_name_edit);
            vert_box.append(&task_tags_edit);
            labels_box.append(&start_label);
            labels_box.append(&stop_label);
            times_box.append(&start_time_edit);
            times_box.append(&stop_time_edit);
            vert_box.append(&labels_box);
            vert_box.append(&times_box);
            vert_box.append(&instructions);
            vert_box.append(&time_error);
            vert_box.append(&future_error);
            vert_box.append(&name_error);
            message_area.append(&vert_box);

            dialog.connect_response(clone!(@strong dialog => move |_ , resp| {
                if resp == gtk::ResponseType::Ok {
                    instructions.set_visible(false);
                    time_error.set_visible(false);
                    future_error.set_visible(false);
                    name_error.set_visible(false);
                    let mut do_not_close = false;
                    let mut new_start_time_local = Local::now();
                    let mut new_stop_time_local = Local::now();

                    // Task Name
                    if task_name_edit.text().trim().is_empty() {
                        name_error.set_visible(true);
                        do_not_close = true;
                    }

                    // Start Time
                    let new_start_time_str = start_time_edit.text();
                    let new_start_time: Result<NaiveDateTime, ParseError>;
                    if settings_manager::get_bool("show-seconds") {
                        new_start_time = NaiveDateTime::parse_from_str(
                                            &new_start_time_str,
                                            "%h %d %Y %H:%M:%S");
                    } else {
                        new_start_time = NaiveDateTime::parse_from_str(
                                                &new_start_time_str,
                                                "%h %d %Y %H:%M");
                    }
                    if let Err(_) = new_start_time {
                        instructions.set_visible(true);
                        do_not_close = true;
                    } else {
                        new_start_time_local = Local.from_local_datetime(&new_start_time.unwrap()).unwrap();
                        if (Local::now() - new_start_time_local).num_seconds() < 0 {
                            future_error.set_visible(true);
                            do_not_close = true;
                        }
                    }

                    // Stop Time
                    let new_stop_time_str = stop_time_edit.text();
                    let new_stop_time: Result<NaiveDateTime, ParseError>;
                    if settings_manager::get_bool("show-seconds") {
                        new_stop_time = NaiveDateTime::parse_from_str(
                                            &new_stop_time_str,
                                            "%h %d %Y %H:%M:%S");
                    } else {
                        new_stop_time = NaiveDateTime::parse_from_str(
                                                &new_stop_time_str,
                                                "%h %d %Y %H:%M");
                    }
                    if let Err(_) = new_stop_time {
                        instructions.set_visible(true);
                        do_not_close = true;
                    } else {
                        new_stop_time_local = Local.from_local_datetime(&new_stop_time.unwrap()).unwrap();
                        if (Local::now() - new_stop_time_local).num_seconds() < 0 {
                            future_error.set_visible(true);
                            do_not_close = true;
                        }
                    }

                    // Tags
                    let mut new_tag_list = "".to_string();
                    if !task_tags_edit.text().trim().is_empty() {
                        let new_tags = task_tags_edit.text();
                        let mut split_tags: Vec<&str> = new_tags.trim().split("#").collect();
                        split_tags = split_tags.iter().map(|x| x.trim()).collect();
                        // Don't allow empty tags
                        split_tags.retain(|&x| !x.trim().is_empty());
                        // Handle duplicate tags before they are saved
                        split_tags = split_tags.into_iter().unique().collect();
                        // Lowercase tags
                        let lower_tags: Vec<String> = split_tags.iter().map(|x| x.to_lowercase()).collect();
                        new_tag_list = lower_tags.join(" #");
                    }

                    if !do_not_close {
                        let _ = database::db_write(task_name_edit.text().trim(),
                                                    new_start_time_local,
                                                    new_stop_time_local,
                                                    new_tag_list);
                        this.reset_history_box();
                        dialog.close();
                    }

                } else if resp == gtk::ResponseType::Cancel {
                    dialog.close();
                }
            }));

            dialog.show();
        }));
    }

    fn setup_settings(&self) {
        let imp = imp::FurtheranceWindow::from_instance(self);
        self.reset_idle();

        // Enter starts timer
        let start = imp.start_button.clone();
        self.set_default_widget(Some(&start));
        imp.task_input.set_activates_default(true);
    }

    fn get_idle_time(&self) -> Result<u64, Box<dyn std::error::Error>> {
        let c = Connection::new_session()?;

        let p = c.with_proxy("org.gnome.Mutter.IdleMonitor",
            "/org/gnome/Mutter/IdleMonitor/Core",
            Duration::from_millis(5000)
        );
        let (idle_time,): (u64,) = p.method_call("org.gnome.Mutter.IdleMonitor", "GetIdletime", ())?;

        Ok(idle_time / 1000)
    }

    fn check_user_idle(&self) {
        let imp = imp::FurtheranceWindow::from_instance(self);
        // Check for user idle
        let idle_time = match self.get_idle_time() {
            Ok(val) => val,
            Err(_) => 1,
        };
        // If user was idle and has now returned...
        if idle_time < (settings_manager::get_int("idle-time") * 60) as u64
            && *imp.idle_time_reached.lock().unwrap()
            && !*imp.idle_notified.lock().unwrap() {

                *imp.idle_notified.lock().unwrap() = true;
                self.resume_from_idle();
        }
        *imp.stored_idle.lock().unwrap() = idle_time;

        // If user is idle but has not returned...
        if *imp.stored_idle.lock().unwrap() >= (settings_manager::get_int("idle-time") * 60) as u64
            && !*imp.idle_time_reached.lock().unwrap() {

            *imp.idle_time_reached.lock().unwrap() = true;
            let true_idle_start_time = Local::now() -
                ChronDur::seconds((settings_manager::get_int("idle-time") * 60) as i64);
            *imp.idle_start_time.lock().unwrap() = true_idle_start_time.to_rfc3339();
        }
    }

    fn resume_from_idle(&self) {
        let imp = imp::FurtheranceWindow::from_instance(self);

        let resume_time = Local::now();
        let idle_start = DateTime::parse_from_rfc3339(&imp.idle_start_time.lock().unwrap()).unwrap();
        let idle_start = idle_start.with_timezone(&Local);
        let idle_time = resume_time - idle_start;
        let idle_time = idle_time.num_seconds();
        let h = idle_time / 60 / 60;
        let m = (idle_time / 60) - (h * 60);
        let s = idle_time - (m * 60);
        let idle_time_str = format!("{}{:02}:{:02}:{:02}", gettext("You have been idle for "), h, m, s);
        let question_str = gettext("\nWould you like to discard that time, or continue the clock?");
        let idle_time_msg = format!("{}{}", idle_time_str, question_str);

        let dialog = gtk::MessageDialog::with_markup(
            Some(self),
            gtk::DialogFlags::MODAL,
            gtk::MessageType::Warning,
            gtk::ButtonsType::None,
            Some(&format!("<span size='x-large' weight='bold'>{}</span>", &gettext("Idle"))),
        );
        dialog.add_buttons(&[
            (&gettext("Discard"), gtk::ResponseType::Reject),
            (&gettext("Continue"), gtk::ResponseType::Accept)
        ]);
        dialog.set_secondary_text(Some(&idle_time_msg));

        dialog.connect_response(clone!(
            @weak self as this,
            @strong dialog,
            @strong imp.start_button as start_button => move |_, resp| {
            if resp == gtk::ResponseType::Reject {
                this.set_subtract_idle(true);
                start_button.emit_clicked();
                dialog.close();
            } else if resp == gtk::ResponseType::Accept {
                this.reset_idle();
                dialog.close();
            }
        }));

        *imp.idle_dialog.lock().unwrap() = dialog.clone();
        let app = FurtheranceApplication::default();
        app.system_idle_notification(&idle_time_str, &question_str);

        dialog.show();
    }

    fn pomodoro_over(&self, timer_start: DateTime<Local>, timer_stop: DateTime<Local>) {
        let dialog = gtk::MessageDialog::with_markup(
            Some(self),
            gtk::DialogFlags::MODAL,
            gtk::MessageType::Warning,
            gtk::ButtonsType::None,
            Some(&format!("<span size='x-large' weight='bold'>{}</span>", &gettext("Time's up!"))),
        );
        dialog.add_buttons(&[
            (&gettext("Continue"), gtk::ResponseType::Accept),
            (&gettext("Stop"), gtk::ResponseType::Reject)
        ]);

        let app = FurtheranceApplication::default();
        app.system_pomodoro_notification(dialog.clone());
        dialog.connect_response(clone!(
            @weak self as this,
            @strong dialog => move |_, resp| {
            let imp = imp::FurtheranceWindow::from_instance(&this);
            if resp == gtk::ResponseType::Reject {
                imp.start_button.set_icon_name("media-playback-start-symbolic");
                this.refresh_timer();
                imp.task_input.set_sensitive(true);
                this.save_task(timer_start, timer_stop);
                this.reset_idle();
                dialog.close();
            } else if resp == gtk::ResponseType::Accept {
                *imp.pomodoro_continue.lock().unwrap() = true;
                this.reset_idle();
                imp.start_button.emit_clicked();
                dialog.close();
            }
        }));

        let imp2 = imp::FurtheranceWindow::from_instance(self);
        imp2.idle_dialog.lock().unwrap().close();

        dialog.show();
    }

    fn write_autosave(&self, auto_start_time: DateTime<Local>) {
        let auto_stop_time = Local::now().to_rfc3339();
        let auto_start_time = auto_start_time.to_rfc3339();
        let (task_name, tag_list) = self.split_tags_and_task();

        let path = FurtheranceWindow::get_autosave_path();
        let file = File::create(path).expect("Couldn't create autosave file");
        let mut file = BufWriter::new(file);

        writeln!(file, "{}", task_name).expect("Unable to write autosave");
        writeln!(file, "{}", auto_start_time).expect("Unable to write autosave");
        writeln!(file, "{}", auto_stop_time).expect("Unable to write autosave");
        write!(file, "{}", tag_list).expect("Unable to write autosave");
    }

    fn delete_autosave() {
        let path = FurtheranceWindow::get_autosave_path();
        if path.exists() {
            remove_file(path).expect("Could not delete autosave");
        }
    }

    fn get_autosave_path() -> PathBuf {
        let mut path = PathBuf::new();
        if let Some(proj_dirs) = ProjectDirs::from("com", "lakoliu",  "Furtherance") {
            path = PathBuf::from(proj_dirs.data_dir());
            create_dir_all(path.clone()).expect("Unable to create autosave directory");
            path.extend(&["furtherance_autosave.txt"]);
        }
        path
    }

    fn split_tags_and_task(&self) -> (String, String) {
        let imp = imp::FurtheranceWindow::from_instance(self);
        let task_input_text = imp.task_input.text();
        let mut split_tags: Vec<&str> = task_input_text.trim().split("#").collect();
        // Remove task name from tags list
        let task_name = *split_tags.first().unwrap();
        split_tags.remove(0);
        // Trim whitespace around each tag
        split_tags = split_tags.iter().map(|x| x.trim()).collect();
        // Don't allow empty tags
        split_tags.retain(|&x| !x.trim().is_empty());
        // Handle duplicate tags before they are ever saved
        split_tags = split_tags.into_iter().unique().collect();
        // Lowercase tags
        let lower_tags: Vec<String> = split_tags.iter().map(|x| x.to_lowercase()).collect();
        let tag_list = lower_tags.join(" #");
        (task_name.trim().to_string(), tag_list)
    }

    fn check_for_autosave(&self) {
        let path = FurtheranceWindow::get_autosave_path();
        if path.exists() {
            let autosave = FurtheranceWindow::read_autosave().unwrap();

            database::write_autosave(&autosave[0], &autosave[1], &autosave[2], &autosave[3])
                .expect("Could not write autosave");

            let dialog = gtk::MessageDialog::new(
                Some(self),
                gtk::DialogFlags::MODAL,
                gtk::MessageType::Info,
                gtk::ButtonsType::Ok,
                &gettext("Autosave Restored"),
            );
            dialog.set_secondary_text(Some(
                &gettext("Furtherance shut down improperly. An autosave was restored.")
            ));

            dialog.connect_response(clone!(
                @weak self as this,
                @strong dialog => move |_, resp| {
                if resp == gtk::ResponseType::Ok {
                    this.reset_history_box();
                    dialog.close();
                }
            }));

            dialog.show();
            FurtheranceWindow::delete_autosave();
        }
    }

    fn read_autosave() -> io::Result<Vec<String>> {
        let path = FurtheranceWindow::get_autosave_path();
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut vars = Vec::new();

        for line in reader.lines() {
            vars.push(line?);
        }
        // Add empty string if there are no tags
        if vars.len() == 3 {
            vars.push("".to_string());
        }

        Ok(vars)
    }

    pub fn reset_idle(&self) {
        let imp = imp::FurtheranceWindow::from_instance(self);
        *imp.stored_idle.lock().unwrap() = 0;
        *imp.idle_notified.lock().unwrap() = false;
        *imp.idle_time_reached.lock().unwrap() = false;
        *imp.subtract_idle.lock().unwrap() = false;
    }

    pub fn set_subtract_idle(&self, val: bool) {
        let imp = imp::FurtheranceWindow::from_instance(self);
        *imp.subtract_idle.lock().unwrap() = val;
    }

    pub fn duplicate_task(&self, task: database::Task) {
        let imp = imp::FurtheranceWindow::from_instance(self);
        if !*imp.running.lock().unwrap() {
            let task_text: String;
            if task.tags.trim().is_empty() {
                task_text = task.task_name;
            } else {
                task_text = format!("{} #{}", task.task_name, task.tags);
            }
            imp.task_input.set_text(&task_text);
            imp.start_button.emit_clicked();
        } else {
            self.display_toast(&gettext("Stop the timer to duplicate a task."));
        }
    }

    pub fn refresh_timer(&self) {
        let imp = imp::FurtheranceWindow::from_instance(self);
        if settings_manager::get_bool("pomodoro") {
            let mut mins = settings_manager::get_int("pomodoro-time");
            let mut hrs: i32 = 0;
            if mins > 59 {
                hrs = mins / 60;
                mins = mins % 60;
            }
            let watch_text: &str = &format!("{:02}:{:02}:00", hrs, mins);
            imp.watch.set_text(watch_text);
        } else {
            imp.watch.set_text("00:00:00");
        }
    }
}

impl Default for FurtheranceWindow {
    fn default() -> Self {
        FurtheranceApplication::default()
            .active_window()
            .unwrap()
            .downcast()
            .unwrap()
    }
}

