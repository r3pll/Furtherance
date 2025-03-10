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

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, CompositeTemplate};
use glib::subclass;

use crate::ui::{FurtheranceWindow, FurTasksPage};
use crate::FurtheranceApplication;
use crate::database;
use crate::config;

enum View {
    Loading,
    Empty,
    Tasks,
}

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/com/lakoliu/Furtherance/gtk/history_box.ui")]
    pub struct FurHistoryBox {
        // Template widgets
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub spinner: TemplateChild<gtk::Spinner>,
        #[template_child]
        pub welcome_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub tasks_page: TemplateChild<FurTasksPage>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FurHistoryBox {
        const NAME: &'static str = "FurHistoryBox";
        type Type = super::FurHistoryBox;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
        }

        fn instance_init(obj: &subclass::InitializingObject<Self>) {
            obj.init_template();
        }

    }

    impl ObjectImpl for FurHistoryBox {
        fn constructed(&self, obj: &Self::Type) {
            obj.setup_widgets();
            self.parent_constructed(obj);
        }

    }
    impl WidgetImpl for FurHistoryBox {}
    impl BoxImpl for FurHistoryBox {}
}

glib::wrapper! {
    pub struct FurHistoryBox(
        ObjectSubclass<imp::FurHistoryBox>)
        @extends gtk::Widget, gtk::Box;
}


impl FurHistoryBox {
    fn setup_widgets(&self) {
        self.set_view(View::Loading);
        let is_saved_task: bool = match database::check_for_tasks() {
            Ok(_) => true,
            Err(_) => false,
        };
        if is_saved_task {
            self.set_view(View::Tasks);
        } else {
            self.set_view(View::Empty);
        }

        // Change "empty" page icon for development mode
        let imp = imp::FurHistoryBox::from_instance(self);
        if config::PROFILE == "development" {
            imp.welcome_page.set_icon_name(Some("com.lakoliu.Furtherance.Devel"));
        }

    }

    fn set_view(&self, view: View) {
        let imp = imp::FurHistoryBox::from_instance(self);
        let app = FurtheranceApplication::default();
        app.delete_enabled(false);
        imp.spinner.set_spinning(false);

        let name = match view {
            View::Loading => {
                imp.spinner.set_spinning(true);
                "loading"
            }
            View::Empty => "empty",
            View::Tasks => {
                app.delete_enabled(true);
                "tasks"
            }
        };

        imp.stack.set_visible_child_name(name);
    }

    pub fn create_tasks_page(&self) {
        let imp = imp::FurHistoryBox::from_instance(self);
        let window = FurtheranceWindow::default();
        imp.tasks_page.clear_task_list();
        let is_saved_task: bool = match database::check_for_tasks() {
            Ok(_) => true,
            Err(_) => false,
        };
        if is_saved_task {
            self.set_view(View::Loading);
            imp.tasks_page.build_task_list();
            self.set_view(View::Tasks);
            window.set_height_request(300);
        } else {
            self.set_view(View::Empty);
            window.set_height_request(390);
        }
    }

    pub fn empty_view(&self) {
        self.set_view(View::Empty);
        let window = FurtheranceWindow::default();
        window.set_height_request(390);
    }

}
