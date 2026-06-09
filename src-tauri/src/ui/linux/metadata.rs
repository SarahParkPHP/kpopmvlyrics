#![cfg(desktop_unix)]

//! "Metadata" tab: edit the song's group/artist, agency, source/copyright,
//! release date and languages, plus the member and featured-artist rosters
//! (each with a name, image and color).
//!
//! The static text fields are read on Save; the member/featured rosters are
//! edited through working buffers and rebuilt into rows on demand. Save writes
//! everything back into `model.song` and persists it (song row + member
//! overrides + featured artists JSON) via [`AppContext::save_song_metadata`].

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk::RGBA;
use gtk::prelude::*;
use gtk::{
    ApplicationWindow, Box as GtkBox, Button, ColorDialog, ColorDialogButton, Entry, Label,
    Orientation, ScrolledWindow,
};

use crate::models::MemberProfile;

use super::{spawn_work, BackgroundUpdate, UiModel, UiView};

/// Stack page name for the metadata tab.
pub const METADATA_PAGE: &str = "metadata";

pub struct MetadataPanel {
    pub root: ScrolledWindow,
    group_artist: Entry,
    agency: Entry,
    source_url: Entry,
    copyright: Entry,
    release_date: Entry,
    primary_language: Entry,
    secondary_languages: Entry,
    members_list: GtkBox,
    featured_list: GtkBox,
    add_member: Button,
    add_featured: Button,
    save_button: Button,
    members: Rc<RefCell<Vec<MemberProfile>>>,
    featured: Rc<RefCell<Vec<MemberProfile>>>,
}

impl MetadataPanel {
    pub fn new() -> Rc<Self> {
        let form = GtkBox::new(Orientation::Vertical, 10);
        form.set_margin_top(12);
        form.set_margin_bottom(12);
        form.set_margin_start(12);
        form.set_margin_end(12);

        let group_artist = entry("Group / artist name");
        let agency = entry("Agency");
        let source_url = entry("https://…");
        let copyright = entry("Copyright");
        let release_date = entry("YYYY-MM-DD");
        let primary_language = entry("e.g. Korean");
        let secondary_languages = entry("Comma-separated, e.g. English, Japanese");

        form.append(&section_label("Song"));
        form.append(&labeled_row("Group / Artist", &group_artist));
        form.append(&labeled_row("Agency", &agency));
        form.append(&labeled_row("Source URL", &source_url));
        form.append(&labeled_row("Copyright", &copyright));
        form.append(&labeled_row("Release date", &release_date));
        form.append(&labeled_row("Primary language", &primary_language));
        form.append(&labeled_row("Secondary languages", &secondary_languages));

        let members_header = GtkBox::new(Orientation::Horizontal, 8);
        members_header.append(&section_label("Members"));
        let add_member = Button::with_label("+ Add member");
        add_member.set_halign(gtk::Align::Start);
        members_header.append(&add_member);
        form.append(&members_header);
        let members_list = GtkBox::new(Orientation::Vertical, 6);
        form.append(&members_list);

        let featured_header = GtkBox::new(Orientation::Horizontal, 8);
        featured_header.append(&section_label("Featured artists"));
        let add_featured = Button::with_label("+ Add featured");
        add_featured.set_halign(gtk::Align::Start);
        featured_header.append(&add_featured);
        form.append(&featured_header);
        let featured_list = GtkBox::new(Orientation::Vertical, 6);
        form.append(&featured_list);

        let save_button = Button::with_label("Save metadata");
        save_button.add_css_class("suggested-action");
        save_button.set_halign(gtk::Align::Start);
        save_button.set_margin_top(6);
        form.append(&save_button);

        let root = ScrolledWindow::new();
        root.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        root.set_vexpand(true);
        root.set_child(Some(&form));

        Rc::new(Self {
            root,
            group_artist,
            agency,
            source_url,
            copyright,
            release_date,
            primary_language,
            secondary_languages,
            members_list,
            featured_list,
            add_member,
            add_featured,
            save_button,
            members: Rc::new(RefCell::new(Vec::new())),
            featured: Rc::new(RefCell::new(Vec::new())),
        })
    }

    /// Wire the add-row and Save buttons. `window` is needed for the image
    /// file picker used by member rows.
    pub fn connect(
        self: &Rc<Self>,
        view: &Rc<UiView>,
        window: &ApplicationWindow,
        work_tx: std::sync::mpsc::Sender<BackgroundUpdate>,
    ) {
        {
            let this = Rc::clone(self);
            let window = window.clone();
            self.add_member.connect_clicked(move |_| {
                this.members.borrow_mut().push(blank_profile("New member"));
                this.rebuild_members(&window);
            });
        }
        {
            let this = Rc::clone(self);
            let window = window.clone();
            self.add_featured.connect_clicked(move |_| {
                this.featured.borrow_mut().push(blank_profile("Featured artist"));
                this.rebuild_featured(&window);
            });
        }
        {
            let this = Rc::clone(self);
            let view = Rc::clone(view);
            self.save_button.connect_clicked(move |_| {
                this.save(&view, work_tx.clone());
            });
        }
    }

    /// Load the current song's metadata into the form. Called when the tab
    /// becomes visible so in-progress edits aren't clobbered by refresh ticks.
    pub fn populate(self: &Rc<Self>, view: &Rc<UiView>, window: &ApplicationWindow) {
        let Ok(model) = view.model.try_borrow() else {
            return;
        };
        match model.song.as_ref() {
            Some(package) => {
                let song = &package.song;
                let group_artist = song
                    .group_name
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| song.artist.clone());
                self.group_artist.set_text(&group_artist);
                self.agency.set_text(song.agency.as_deref().unwrap_or(""));
                self.source_url
                    .set_text(song.source_url.as_deref().unwrap_or(""));
                self.copyright
                    .set_text(song.copyright.as_deref().unwrap_or(""));
                self.release_date
                    .set_text(song.release_date.as_deref().unwrap_or(""));
                self.primary_language
                    .set_text(song.primary_language.as_deref().unwrap_or(""));
                self.secondary_languages
                    .set_text(&song.secondary_languages.join(", "));
                *self.members.borrow_mut() = package.members.clone();
                *self.featured.borrow_mut() = song.featured_artists.clone();
            }
            None => {
                for entry in [
                    &self.group_artist,
                    &self.agency,
                    &self.source_url,
                    &self.copyright,
                    &self.release_date,
                    &self.primary_language,
                    &self.secondary_languages,
                ] {
                    entry.set_text("");
                }
                self.members.borrow_mut().clear();
                self.featured.borrow_mut().clear();
            }
        }
        drop(model);
        self.rebuild_members(window);
        self.rebuild_featured(window);
    }

    fn save(self: &Rc<Self>, view: &Rc<UiView>, work_tx: std::sync::mpsc::Sender<BackgroundUpdate>) {
        let group_artist = some_if_filled(self.group_artist.text().to_string());
        let agency = some_if_filled(self.agency.text().to_string());
        let source_url = some_if_filled(self.source_url.text().to_string());
        let copyright = some_if_filled(self.copyright.text().to_string());
        let release_date = some_if_filled(self.release_date.text().to_string());
        let primary_language = some_if_filled(self.primary_language.text().to_string());
        let secondary_languages = parse_languages(&self.secondary_languages.text());
        let members = self.members.borrow().clone();
        let featured = self.featured.borrow().clone();

        let mut has_song = false;
        view.refresh_mut(|model| {
            if let Some(package) = model.song.as_mut() {
                has_song = true;
                // Group and artist are one concept here: keep them in sync and
                // never blank them out from an empty field.
                if let Some(name) = group_artist.clone() {
                    package.song.artist = name.clone();
                    package.song.group_name = Some(name);
                }
                package.song.source_url = source_url.clone();
                package.song.agency = agency.clone();
                package.song.copyright = copyright.clone();
                package.song.release_date = release_date.clone();
                package.song.primary_language = primary_language.clone();
                package.song.secondary_languages = secondary_languages.clone();
                package.song.featured_artists = featured.clone();
                package.members = members.clone();
            } else {
                model.error = Some("Load or import a song before editing metadata".to_string());
            }
        });
        if !has_song {
            return;
        }

        spawn_work(work_tx, Rc::clone(view), "Metadata", move |snapshot| {
            let mut package = snapshot
                .song
                .clone()
                .ok_or_else(|| "Load or import a song before editing metadata".to_string())?;
            snapshot.ctx.save_song_metadata(&mut package)?;
            Ok(Box::new(move |model: &mut UiModel| {
                model.song = Some(package);
                model.editor_table_dirty = true;
            }) as Box<dyn FnOnce(&mut UiModel) + Send>)
        });
    }

    fn rebuild_members(self: &Rc<Self>, window: &ApplicationWindow) {
        clear_box(&self.members_list);
        let count = self.members.borrow().len();
        for index in 0..count {
            let row = self.build_profile_row(Rc::clone(&self.members), index, window, true);
            self.members_list.append(&row);
        }
    }

    fn rebuild_featured(self: &Rc<Self>, window: &ApplicationWindow) {
        clear_box(&self.featured_list);
        let count = self.featured.borrow().len();
        for index in 0..count {
            let row = self.build_profile_row(Rc::clone(&self.featured), index, window, false);
            self.featured_list.append(&row);
        }
    }

    fn build_profile_row(
        self: &Rc<Self>,
        buffer: Rc<RefCell<Vec<MemberProfile>>>,
        index: usize,
        window: &ApplicationWindow,
        is_member: bool,
    ) -> GtkBox {
        let current = buffer.borrow()[index].clone();
        let row = GtkBox::new(Orientation::Horizontal, 6);

        let name = Entry::new();
        name.set_placeholder_text(Some("Name"));
        name.set_text(&current.stage_name);
        name.set_hexpand(true);
        {
            let buffer = Rc::clone(&buffer);
            name.connect_changed(move |entry| {
                if let Some(member) = buffer.borrow_mut().get_mut(index) {
                    member.stage_name = entry.text().to_string();
                }
            });
        }
        row.append(&name);

        let color_button = ColorDialogButton::new(Some(ColorDialog::new()));
        if let Ok(rgba) = RGBA::parse(&current.color) {
            color_button.set_rgba(&rgba);
        }
        {
            let buffer = Rc::clone(&buffer);
            color_button.connect_rgba_notify(move |button| {
                if let Some(member) = buffer.borrow_mut().get_mut(index) {
                    member.color = rgba_to_hex(button.rgba());
                }
            });
        }
        row.append(&color_button);

        let image_label = Label::new(Some(&image_caption(&current)));
        image_label.set_width_chars(16);
        image_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        image_label.set_xalign(0.0);
        row.append(&image_label);

        let image_button = Button::with_label("Image…");
        {
            let buffer = Rc::clone(&buffer);
            let window = window.clone();
            let image_label = image_label.clone();
            image_button.connect_clicked(move |_| {
                pick_profile_image(&window, Rc::clone(&buffer), index, image_label.clone());
            });
        }
        row.append(&image_button);

        let remove = Button::from_icon_name("user-trash-symbolic");
        remove.set_tooltip_text(Some("Remove"));
        remove.add_css_class("destructive-action");
        {
            let this = Rc::clone(self);
            let buffer = Rc::clone(&buffer);
            let window = window.clone();
            remove.connect_clicked(move |_| {
                {
                    let mut items = buffer.borrow_mut();
                    if index < items.len() {
                        items.remove(index);
                    }
                }
                if is_member {
                    this.rebuild_members(&window);
                } else {
                    this.rebuild_featured(&window);
                }
            });
        }
        row.append(&remove);

        row
    }
}

fn pick_profile_image(
    window: &ApplicationWindow,
    buffer: Rc<RefCell<Vec<MemberProfile>>>,
    index: usize,
    image_label: Label,
) {
    let dialog = gtk::FileDialog::builder()
        .title("Choose image")
        .accept_label("_Open")
        .modal(true)
        .build();

    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Images"));
    filter.add_mime_type("image/jpeg");
    filter.add_mime_type("image/png");
    filter.add_mime_type("image/gif");
    filter.add_mime_type("image/webp");
    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    dialog.set_filters(Some(&filters));
    dialog.set_default_filter(Some(&filter));

    dialog.open(
        Some(window),
        None::<&gtk::gio::Cancellable>,
        move |result| {
            if let Ok(file) = result {
                if let Some(path) = file.path() {
                    let path = path.to_string_lossy().into_owned();
                    if let Some(member) = buffer.borrow_mut().get_mut(index) {
                        member.local_image_path = Some(path.clone());
                    }
                    image_label.set_text(&basename(&path));
                }
            }
        },
    );
}

fn entry(placeholder: &str) -> Entry {
    let entry = Entry::new();
    entry.set_placeholder_text(Some(placeholder));
    entry.set_hexpand(true);
    entry
}

fn labeled_row(label: &str, entry: &Entry) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 8);
    let label = Label::new(Some(label));
    label.set_width_chars(18);
    label.set_xalign(0.0);
    row.append(&label);
    row.append(entry);
    row
}

fn section_label(text: &str) -> Label {
    let label = Label::new(Some(text));
    label.set_xalign(0.0);
    label.add_css_class("heading");
    label.set_margin_top(6);
    label
}

fn blank_profile(name: &str) -> MemberProfile {
    MemberProfile {
        id: None,
        stage_name: name.to_string(),
        real_name: None,
        color: "#888888".to_string(),
        image_url: None,
        local_image_path: None,
        provider: None,
    }
}

fn clear_box(container: &GtkBox) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn some_if_filled(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_languages(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .map(|part| part.to_string())
        .collect()
}

fn image_caption(member: &MemberProfile) -> String {
    member
        .local_image_path
        .as_deref()
        .or(member.image_url.as_deref())
        .map(basename)
        .unwrap_or_else(|| "No image".to_string())
}

fn basename(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn rgba_to_hex(rgba: RGBA) -> String {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(rgba.red()),
        channel(rgba.green()),
        channel(rgba.blue())
    )
}
