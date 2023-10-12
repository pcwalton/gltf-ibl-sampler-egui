// gltf-ibl-sampler-egui/src/main.rs

#![allow(non_upper_case_globals)]

use crate::generator::{FilterSettings, Job, OutputProgress};
use eframe::{self, App, CreationContext, Frame as EFrame, IconData, NativeOptions, Storage};
use egui::text::LayoutJob;
use egui::{
    Align, Button, CentralPanel, CollapsingHeader, Color32, ColorImage, ComboBox, Context,
    FontFamily, FontId, Grid, Id, Layout, ProgressBar, RichText, ScrollArea, TextEdit, TextFormat,
    TextureHandle, TextureOptions, TopBottomPanel, Ui, Vec2, Window,
};
use generator::{Distribution, InputReencodingStatus, Output, TargetFormat};
use image::imageops::FilterType;
use log::{warn, Level, LevelFilter, Log, Metadata, Record};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageLevel};
use rust_i18n::t;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt::{Display, Write};
use std::iter;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::thread;

rust_i18n::i18n!("locales");

#[allow(non_camel_case_types, non_upper_case_globals)]
mod bindgen {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

mod generator;

// Internally, the image preview is stored at this resolution to save on VRAM.
const INTERNALIMAGE_PREVIEW_HEIGHT: u32 = 480;

const DEFAULT_IMAGE_PREVIEW_HEIGHT: f32 = 128.0;

static INITIAL_WINDOW_SIZE: Vec2 = Vec2::new(480.0, 640.0);

static ICON_PNG_DATA: &[u8] = include_bytes!("../Icon.png");

struct IblSamplerApp {
    job: Job,
    input_preview: Arc<Mutex<InputPreview>>,
    output_progress: Arc<Mutex<OutputProgress>>,
    just_loaded: bool,
    top_panel_resized_by_user: bool,
    log_window_open: bool,
}

struct InputPreview {
    payload: InputPreviewPayload,
    // Avoids race conditions.
    epoch: usize,
}

enum InputPreviewPayload {
    NoneSelected,
    Loading,
    Loaded(TextureHandle),
}

struct LogBuffer(Mutex<LogBufferImpl>);

struct LogBufferImpl {
    lines: Vec<RichText>,
    ctx: Option<Context>,
}

static LOG_BUFFER: LogBuffer = LogBuffer(Mutex::new(LogBufferImpl {
    lines: Vec::new(),
    ctx: None,
}));

fn main() {
    drop(log::set_logger(&LOG_BUFFER));
    log::set_max_level(LevelFilter::Info);

    let native_options = NativeOptions {
        initial_window_size: Some(INITIAL_WINDOW_SIZE),
        drag_and_drop_support: true,
        app_id: Some("GLTFIBLSampler".to_owned()),
        icon_data: IconData::try_from_png_bytes(ICON_PNG_DATA).ok(),
        ..NativeOptions::default()
    };

    eframe::run_native(
        &t!("app.title"),
        native_options,
        Box::new(IblSamplerApp::create),
    )
    .unwrap();
}

impl IblSamplerApp {
    fn create(ctx: &CreationContext) -> Box<dyn App> {
        let job = ctx
            .storage
            .and_then(|storage| storage.get_string("job"))
            .and_then(|encoded_job| ron::from_str(&encoded_job).ok())
            .unwrap_or_default();

        Box::new(IblSamplerApp {
            job,
            input_preview: Arc::new(Mutex::new(InputPreview {
                payload: InputPreviewPayload::NoneSelected,
                epoch: 0,
            })),
            output_progress: Arc::new(Mutex::new(OutputProgress::NotStartedYet)),
            just_loaded: true,
            top_panel_resized_by_user: false,
            log_window_open: false,
        })
    }
}

impl App for IblSamplerApp {
    fn update(&mut self, ctx: &Context, _: &mut EFrame) {
        if self.just_loaded {
            self.just_loaded = false;
            self.load_input_preview(ctx);

            if let Ok(mut lock) = LOG_BUFFER.0.lock() {
                lock.ctx = Some((*ctx).clone());
            }
        }

        let mut files_changed = false;

        TopBottomPanel::top("IblTopPanel")
            .resizable(true)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| files_changed = self.input_ui(ui) || files_changed);
            });

        // FIXME: This is a pretty ugly way to detect resizes…
        self.top_panel_resized_by_user = self.top_panel_resized_by_user
            || ctx
                .memory(|memory| memory.is_being_dragged(Id::new("IblTopPanel").with("__resize")));

        TopBottomPanel::bottom("IblBottomPanel")
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.vertical(|ui| self.bottom_panel_ui(ui));
            });

        CentralPanel::default().show(ctx, |ui| {
            files_changed = self.outputs_ui(ui) || files_changed
        });

        ctx.input(|input| {
            if let Some(new_path) = input
                .raw
                .dropped_files
                .first()
                .and_then(|path| path.path.clone())
            {
                self.set_input_path(ctx, new_path);
                files_changed = true;
            }
        });

        if files_changed {
            self.update_output_paths();
        }

        // Log window
        let mut log_window_open = self.log_window_open;
        Window::new(&t!("log.window.title"))
            .open(&mut log_window_open)
            .default_open(false)
            .scroll2([true, true])
            .collapsible(false)
            .show(ctx, |ui| self.log_window_ui(ui));
        self.log_window_open = log_window_open;
    }

    fn save(&mut self, storage: &mut dyn Storage) {
        if let Ok(job) = ron::to_string(&self.job) {
            storage.set_string("job", job)
        }
    }
}

impl IblSamplerApp {
    /// Returns true if any files changed or false otherwise.
    fn outputs_ui(&mut self, ui: &mut Ui) -> bool {
        let mut files_changed = false;
        let mut outputs_to_delete = vec![];

        ScrollArea::vertical()
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for output_index in 0..self.job.outputs.len() {
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        ui.group(|ui| {
                            if output_index > 0
                                && ui
                                    .button("🗑")
                                    .on_hover_text(layout_text_with_code(&t!("help.output.remove")))
                                    .clicked()
                            {
                                outputs_to_delete.push(output_index);
                            }

                            CollapsingHeader::new(t!("output.header", index = (output_index + 1)))
                                .default_open(true)
                                .show(ui, |ui| {
                                    files_changed =
                                        self.output_ui(ui, output_index) || files_changed
                                });
                        });
                    });
                }
            });

        outputs_to_delete.sort();
        for output_to_delete in outputs_to_delete.into_iter().rev() {
            self.job.outputs.remove(output_to_delete);
        }

        files_changed
    }

    /// Returns true if the file changed and false otherwise.
    fn input_ui(&mut self, ui: &mut Ui) -> bool {
        let mut file_changed = false;

        Grid::new("IblInput").num_columns(2).show(ui, |ui| {
            // Input box
            ui.label(&t!("input"));

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .button(&t!("browse"))
                    .on_hover_text(layout_text_with_code(&t!("help.input.file")))
                    .clicked()
                {
                    if let Some(path) = FileDialog::new()
                        .add_filter(&t!("input.file.type"), &["hdr", "exr"])
                        .pick_file()
                    {
                        self.set_input_path(ui.ctx(), path);
                        file_changed = true;
                    }
                }

                let mut input_file = self.job.input_path.display().to_string();
                if ui
                    .add_sized(ui.available_size(), TextEdit::singleline(&mut input_file))
                    .on_hover_text(layout_text_with_code(&t!("help.input.file")))
                    .changed()
                {
                    self.set_input_path(ui.ctx(), PathBuf::from(input_file));
                    file_changed = true;
                }
            });

            ui.end_row();

            // Maximum size
            output_numeric_value_ui(
                ui,
                &mut self.job.max_image_size,
                &t!("input.max.image.size"),
                Some(&t!("help.input.max.image.size")),
            );
        });

        if let Ok(maybe_texture) = self.input_preview.lock() {
            match maybe_texture.payload {
                InputPreviewPayload::NoneSelected => {}
                InputPreviewPayload::Loading => {
                    ui.spinner();
                    ui.label(&t!("input.preview.loading"));
                }
                InputPreviewPayload::Loaded(ref texture_handle) => {
                    let original_size = texture_handle.size_vec2();

                    let mut height = if self.top_panel_resized_by_user {
                        ui.available_height()
                    } else {
                        DEFAULT_IMAGE_PREVIEW_HEIGHT
                    };
                    let mut width = height * original_size.x / original_size.y;

                    // Clamp width and height to available space.
                    if width > ui.available_width() {
                        width = ui.available_width();
                        height = width * original_size.y / original_size.x;
                    }

                    ui.image(texture_handle, Vec2::new(width, height));
                }
            }
        }

        file_changed
    }

    fn bottom_panel_ui(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| self.output_progress_ui(ui));

        ui.horizontal(|ui| {
            if ui
                .button("➕")
                .on_hover_text(layout_text_with_code(&t!("help.output.add")))
                .clicked()
            {
                let new_output = Output::default_for_index(self.job.outputs.len());
                self.job.outputs.push(new_output);
            }

            ui.with_layout(Layout::right_to_left(Align::Max), |ui| {
                // Generate button
                let disabled = matches!(
                    *self.output_progress.lock().unwrap(),
                    OutputProgress::InProgress { .. }
                );
                if ui
                    .add_enabled(!disabled, Button::new(t!("button.generate")))
                    .on_hover_text(t!("help.button.generate"))
                    .clicked()
                    && self.check_for_overwrite_and_prompt_user()
                {
                    generator::generate(ui.ctx(), self.job.clone(), self.output_progress.clone());
                }

                // Reset button
                if ui
                    .button(&t!("button.reset"))
                    .on_hover_text(t!("help.button.reset"))
                    .clicked()
                {
                    self.job = Job::default();
                }

                // Show Log button
                if ui
                    .button(&t!("button.show.log"))
                    .on_hover_text(t!("help.button.show.log"))
                    .clicked()
                {
                    self.log_window_open = true;
                }
            });
        });
    }

    #[allow(clippy::eq_op)]
    fn output_progress_ui(&mut self, ui: &mut Ui) {
        let Ok(output_progress) = self.output_progress.lock() else { return };

        match *output_progress {
            OutputProgress::NotStartedYet => {}

            OutputProgress::Succeeded { output_count } => {
                if output_count == 1 {
                    ui.label(&t!("output.progress.success.single"));
                } else {
                    ui.label(&t!("output.progress.success.multi", count = output_count));
                }
            }

            OutputProgress::InProgress {
                input_reencoding_status,
                outputs_finished,
                output_count,
            } => {
                let mut progress = match input_reencoding_status {
                    InputReencodingStatus::Loading => 0.0 / 3.0,
                    InputReencodingStatus::Resizing => 1.0 / 3.0,
                    InputReencodingStatus::Writing => 2.0 / 3.0,
                    InputReencodingStatus::Reencoded => 3.0 / 3.0,
                };
                progress += outputs_finished as f32;
                ui.add(
                    ProgressBar::new(progress / (output_count + 1) as f32)
                        .show_percentage()
                        .animate(true),
                );
            }

            OutputProgress::Failed {
                which_failed,
                ref error,
            } => {
                ui.colored_label(
                    Color32::RED,
                    &t!(
                        "output.progress.failure",
                        index = (which_failed + 1),
                        error = error
                    ),
                );
            }
        }
    }

    /// Returns true if the files changed and false otherwise.
    fn output_ui(&mut self, ui: &mut Ui, output_index: usize) -> bool {
        let output = &mut self.job.outputs[output_index];

        let mut files_changed = false;

        let grid_id = format!("IblOutput{}", output_index);
        Grid::new(grid_id).num_columns(2).show(ui, |ui| {
            if output_file_picker(
                ui,
                &mut output.out_cubemap.path,
                &t!("output.cubemap"),
                Some(&t!("help.output.cubemap")),
                &[
                    (&*t!("output.file.ktx2"), "ktx2"),
                    (&*t!("output.file.ktx1"), "ktx1"),
                ],
            ) {
                output.out_cubemap.automatic_filename = false;
                files_changed = true;
            }

            // Mipmap levels
            output_optional_numeric_value_ui(
                ui,
                output_index,
                &t!("output.mipmap.levels"),
                "IblMip",
                &mut output.mip_level_count,
                3,
                Some(&t!("help.output.mipmap.levels")),
            );

            // Cubemap resolution
            output_optional_numeric_value_ui(
                ui,
                output_index,
                &t!("output.cubemap.resolution"),
                "IblCubemapResolution",
                &mut output.cubemap_resolution,
                1024,
                Some(&t!("help.output.cubemap.resolution")),
            );

            // Target format
            output_enum(
                ui,
                &mut output.target_format,
                &t!("output.target.format"),
                output_index,
                &[
                    TargetFormat::R8G8B8A8Unorm,
                    TargetFormat::R32G32B32A32Sfloat,
                ],
                Some(&t!("help.output.target.format")),
            );

            // LOD bias
            output_numeric_value_ui(
                ui,
                &mut output.lod_bias,
                &t!("output.lod.bias"),
                Some(&t!("help.output.lod.bias")),
            );

            // Distribution
            output_distribution(ui, output, output_index);

            if let Some(ref mut filter_settings) = output.filter_settings {
                if output_file_picker(
                    ui,
                    &mut filter_settings.out_lut.path,
                    &t!("output.lut"),
                    Some(&t!("help.output.lut")),
                    &[(&*t!("output.file.png"), "png")],
                ) {
                    filter_settings.out_lut.automatic_filename = false;
                    files_changed = true;
                }

                // Sample count
                output_numeric_value_ui(
                    ui,
                    &mut filter_settings.sample_count,
                    &t!("output.sample.count"),
                    Some(&t!("help.output.sample.count")),
                );
            }
        });

        files_changed
    }

    /// NB: When you call this, make sure to set `files_changed` to true.
    fn set_input_path(&mut self, ctx: &Context, input_path: PathBuf) {
        self.job.input_path = input_path;

        self.load_input_preview(ctx);
    }

    fn load_input_preview(&mut self, ctx: &Context) {
        // Early out if this can't possibly succeed.
        if &*self.job.input_path == Path::new("") {
            return;
        }

        let texture_slot = self.input_preview.clone();

        let epoch;
        {
            let mut texture_slot_inner = texture_slot.lock().unwrap();
            epoch = texture_slot_inner.epoch + 1;
            texture_slot_inner.epoch = epoch;
            texture_slot_inner.payload = InputPreviewPayload::Loading;
        }

        let input_path = self.job.input_path.clone();
        let ctx = (*ctx).clone();

        thread::spawn(move || {
            let Ok(mut image) = generator::load_image(&input_path) else {
                warn!("Failed to open preview: {:?}", input_path);
                return
            };

            image = image.resize_exact(
                (INTERNALIMAGE_PREVIEW_HEIGHT as f32 * image.width() as f32 / image.height() as f32)
                    .round() as u32,
                INTERNALIMAGE_PREVIEW_HEIGHT,
                FilterType::Lanczos3,
            );

            let image_buffer = image.to_rgba8();
            let pixels = image_buffer.as_flat_samples();
            let color_image = ColorImage::from_rgba_unmultiplied(
                [image.width() as usize, image.height() as usize],
                pixels.as_slice(),
            );

            let texture_handle =
                ctx.load_texture("IblInput", color_image, TextureOptions::default());
            if let Ok(mut mutex) = texture_slot.lock() {
                if mutex.epoch == epoch {
                    mutex.payload = InputPreviewPayload::Loaded(texture_handle);
                }
            }

            ctx.request_repaint();
        });
    }

    fn update_output_paths(&mut self) {
        let input_path = &self.job.input_path;

        // Determine the output directory.
        let mut output_dir = None;
        for output in &self.job.outputs {
            if !output.out_cubemap.automatic_filename {
                if let Some(dir) = output.out_cubemap.path.parent() {
                    output_dir = Some(dir.to_owned());
                    break;
                }
            }

            if let Some(ref filter_settings) = output.filter_settings {
                if !filter_settings.out_lut.automatic_filename {
                    if let Some(dir) = filter_settings.out_lut.path.parent() {
                        output_dir = Some(dir.to_owned());
                        break;
                    }
                }
            }
        }

        let Some(output_dir) = output_dir
            .or_else(|| input_path.parent().map(|parent| parent.to_owned())) else { return };
        let file_stem = input_path.file_stem().unwrap_or(OsStr::new(""));

        // Determine other filenames.
        let mut used = HashSet::new();
        for output in &mut self.job.outputs {
            let suffix = match output
                .filter_settings
                .as_ref()
                .map(|filter_settings| filter_settings.distribution)
            {
                None => "cubemap",
                Some(Distribution::Lambertian) => "diffuse",
                Some(Distribution::Ggx) => "specular",
                Some(Distribution::Charlie) => "charlie",
            };

            if output.out_cubemap.automatic_filename {
                if let Some(cubemap_path) = create_output_path(
                    &output_dir,
                    file_stem,
                    suffix,
                    &mut used,
                    /*is_lut=*/ false,
                ) {
                    output.out_cubemap.path = cubemap_path;
                }
            }

            if let Some(ref mut filter_settings) = output.filter_settings {
                if filter_settings.out_lut.automatic_filename {
                    if let Some(lut_path) = create_output_path(
                        &output_dir,
                        file_stem,
                        &format!("{}_lut", suffix),
                        &mut used,
                        /*is_lut=*/ true,
                    ) {
                        filter_settings.out_lut.path = lut_path;
                    }
                }
            }
        }
    }

    fn log_window_ui(&mut self, ui: &mut Ui) {
        // Clone the messages so we don't deadlock if egui logs internally.
        let messages = match LOG_BUFFER.0.lock() {
            Err(_) => return,
            Ok(log_buffer) => log_buffer.lines.clone(),
        };

        for message in messages {
            ui.label(message);
        }
    }

    /// Shows a confirmation dialog box if any overwriting is going to occur. Returns true if the
    /// user authorized the change.
    fn check_for_overwrite_and_prompt_user(&mut self) -> bool {
        let mut paths_to_overwrite = vec![];
        for output in &self.job.outputs {
            if output.out_cubemap.path.exists() {
                paths_to_overwrite.push(output.out_cubemap.path.clone());
            }
            if let Some(ref filter_settings) = output.filter_settings {
                if filter_settings.out_lut.path.exists() {
                    paths_to_overwrite.push(filter_settings.out_lut.path.clone());
                }
            }
        }

        if paths_to_overwrite.is_empty() {
            return true;
        }

        let mut text = String::new();
        writeln!(&mut text, "{}", t!("output.overwrite.a")).unwrap();
        for (index, path) in paths_to_overwrite.into_iter().enumerate() {
            writeln!(
                &mut text,
                "{}",
                t!(
                    "output.overwrite.file",
                    index = (index + 1),
                    path = (path.display())
                )
            )
            .unwrap();
        }
        writeln!(&mut text, "{}", t!("output.overwrite.b")).unwrap();

        MessageDialog::new()
            .set_title(&t!("app.title"))
            .set_level(MessageLevel::Warning)
            .set_buttons(MessageButtons::YesNo)
            .set_description(&text)
            .show()
    }
}

fn create_output_path(
    output_dir: &Path,
    file_stem: &OsStr,
    suffix: &str,
    used: &mut HashSet<PathBuf>,
    is_lut: bool,
) -> Option<PathBuf> {
    for index in iter::once(None).chain((0..).map(Some)) {
        let path = output_dir.join(Path::new(&format!(
            "{}_{}{}.{}",
            file_stem.to_string_lossy(),
            suffix,
            match index {
                None => "".to_owned(),
                Some(index) => format!("_{}", index),
            },
            if is_lut { "png" } else { "ktx2" },
        )));

        if used.insert(path.clone()) {
            return Some(path);
        }
    }

    None
}

fn output_optional_numeric_value_ui(
    ui: &mut Ui,
    output_index: usize,
    label: &str,
    id: &str,
    optional_number: &mut Option<u32>,
    default_value: u32,
    tooltip: Option<&str>,
) {
    ui.label(label);

    let response = ui.horizontal(|ui| {
        let mut custom_value = optional_number.is_some();
        let mut combo_box = ComboBox::from_id_source(format!("{}{}", id, output_index));

        combo_box = if custom_value {
            combo_box.selected_text(t!("output.numeric.custom"))
        } else {
            combo_box
                .selected_text(t!("output.numeric.default"))
                .width(ui.available_width())
        };

        combo_box.show_ui(ui, |ui| {
            ui.selectable_value(&mut custom_value, false, &t!("output.numeric.default"));
            ui.selectable_value(&mut custom_value, true, &t!("output.numeric.custom"));
        });

        match (custom_value, &mut *optional_number) {
            (false, Some(_)) => *optional_number = None,
            (true, None) => *optional_number = Some(default_value),
            _ => {}
        }

        if let Some(ref mut custom_value) = *optional_number {
            let mut custom_value_str = custom_value.to_string();
            ui.add_sized(
                ui.available_size(),
                TextEdit::singleline(&mut custom_value_str),
            );

            if let Ok(new_custom_value) = str::parse(&custom_value_str) {
                *custom_value = new_custom_value;
            }
        }
    });

    if let Some(tooltip) = tooltip {
        response
            .response
            .on_hover_text(layout_text_with_code(tooltip));
    }

    ui.end_row();
}

/// Returns true if the file changed or false otherwise.
fn output_file_picker(
    ui: &mut Ui,
    path: &mut PathBuf,
    label: &str,
    tooltip: Option<&str>,
    files: &[(&str, &str)],
) -> bool {
    let mut changed = false;

    ui.label(label);

    let response = ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui.button(&t!("browse")).clicked() {
            let mut dialog = FileDialog::new();
            for (file_type, file_extension) in files {
                dialog = dialog.add_filter(file_type, &[file_extension]);
            }
            if let Some(new_path) = dialog.save_file() {
                *path = new_path;
                changed = true;
            }
        }

        let mut path_str = path.display().to_string();
        if ui
            .add_sized(ui.available_size(), TextEdit::singleline(&mut path_str))
            .changed()
        {
            if let Ok(new_path) = PathBuf::from_str(&path_str) {
                *path = new_path;
                changed = true;
            }
        }
    });

    if let Some(tooltip) = tooltip {
        response
            .response
            .on_hover_text(layout_text_with_code(tooltip));
    }

    ui.end_row();

    changed
}

fn output_enum<T>(
    ui: &mut Ui,
    value: &mut T,
    label: &str,
    index: usize,
    options: &[T],
    tooltip: Option<&str>,
) where
    T: Clone + PartialEq + ToLocalizedString,
{
    ui.label(label);

    let response = ComboBox::from_id_source(format!("Ibl{}{}", label, index))
        .selected_text(layout_text_with_code(&value.to_localized_string()))
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for option in options {
                ui.selectable_value(
                    value,
                    (*option).clone(),
                    layout_text_with_code(&option.to_localized_string()),
                );
            }
        });

    if let Some(tooltip) = tooltip {
        response
            .response
            .on_hover_text(layout_text_with_code(tooltip));
    }

    ui.end_row();
}

fn output_distribution(ui: &mut Ui, output: &mut Output, index: usize) {
    ui.label(&t!("output.distribution"));

    let mut distribution = output
        .filter_settings
        .as_ref()
        .map(|filter_settings| filter_settings.distribution);

    let response = ComboBox::from_id_source(format!("IblOutputDistribution{}", index))
        .selected_text(layout_text_with_code(&distribution.to_localized_string()))
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for option in &[
                None,
                Some(Distribution::Lambertian),
                Some(Distribution::Ggx),
                Some(Distribution::Charlie),
            ] {
                ui.selectable_value(
                    &mut distribution,
                    *option,
                    layout_text_with_code(&option.to_localized_string()),
                );
            }
        });

    let response = response
        .response
        .on_hover_text(layout_text_with_code(&t!("help.output.distribution")));

    if response.changed() {
        output.filter_settings = distribution.map(|distribution| FilterSettings {
            distribution,
            ..FilterSettings::default_for_index(index)
        });
    }

    ui.end_row();
}

fn output_numeric_value_ui<T>(ui: &mut Ui, value: &mut T, label: &str, tooltip: Option<&str>)
where
    T: FromStr + Display,
{
    ui.label(label);

    let mut string = value.to_string();
    let response = ui.add_sized(ui.available_size(), TextEdit::singleline(&mut string));

    if let Some(tooltip) = tooltip {
        response.on_hover_text(layout_text_with_code(tooltip));
    }

    if let Ok(new_value) = str::parse(&string) {
        *value = new_value;
    }

    ui.end_row();
}

impl Log for LogBuffer {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let Ok(mut this) = self.0.lock() else { return };

        let (icon, color) = match record.level() {
            Level::Debug => ("🐛", Color32::GREEN),
            Level::Trace => ("📰", Color32::BLUE),
            Level::Info => ("ℹ", Color32::WHITE),
            Level::Warn => ("⚠", Color32::YELLOW),
            Level::Error => ("🗙", Color32::RED),
        };

        this.lines
            .push(RichText::new(format!("{} {}", icon, record.args().to_string())).color(color));

        if let Some(ref ctx) = this.ctx {
            ctx.request_repaint();
        }
    }

    fn flush(&self) {}
}

/// Lays out text with Markdown-like `code blocks`.
fn layout_text_with_code(text: &str) -> LayoutJob {
    let mut code = false;
    let mut layout = LayoutJob::default();
    for run in text.split('`') {
        let family = if !code {
            FontFamily::Proportional
        } else {
            FontFamily::Monospace
        };
        layout.append(
            run,
            0.0,
            TextFormat {
                font_id: FontId { family, size: 12.0 },
                ..TextFormat::default()
            },
        );
        code = !code;
    }

    layout
}

trait ToLocalizedString {
    fn to_localized_string(&self) -> String;
}
