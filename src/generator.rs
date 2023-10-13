// gltf-ibl-sampler-egui/src/generator.rs

use crate::bindgen::{
    self, IBLLib_Distribution_Charlie, IBLLib_Distribution_GGX, IBLLib_Distribution_Lambertian,
    IBLLib_Distribution_None, IBLLib_OutputFormat_B9G9R9E5_UFLOAT,
    IBLLib_OutputFormat_R16G16B16A16_SFLOAT, IBLLib_OutputFormat_R32G32B32A32_SFLOAT,
    IBLLib_OutputFormat_R8G8B8A8_UNORM, IBLLib_Result, IBLLib_Result_FileNotFound,
    IBLLib_Result_InputPanoramaFileNotFound, IBLLib_Result_InvalidArgument, IBLLib_Result_KtxError,
    IBLLib_Result_ShaderCompilationFailed, IBLLib_Result_ShaderFileNotFound,
    IBLLib_Result_StbError, IBLLib_Result_Success, IBLLib_Result_VulkanError,
    IBLLib_Result_VulkanInitializationFailed,
};
use crate::ToLocalizedString;
use anyhow::Error;
use derive_more::Display;
use egui::Context;
use image::imageops::FilterType;
use image::io::Reader;
use image::{DynamicImage, ImageBuffer};
use log::info;
use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::os::raw::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::{ptr, slice, thread};
use tempfile::{Builder, NamedTempFile};

const DEFAULT_OUTPUT_COUNT: usize = 3;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Job {
    pub(crate) input_path: PathBuf,
    pub(crate) max_image_size: u32,
    pub(crate) outputs: Vec<Output>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Output {
    pub(crate) out_cubemap: OutputPath,
    pub(crate) mip_level_count: Option<u32>,
    pub(crate) cubemap_resolution: Option<u32>,
    pub(crate) target_format: TargetFormat,
    pub(crate) lod_bias: f32,
    pub(crate) filter_settings: Option<FilterSettings>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct FilterSettings {
    pub(crate) distribution: Distribution,
    pub(crate) out_lut: OutputPath,
    pub(crate) sample_count: u32,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct OutputPath {
    pub(crate) path: PathBuf,
    pub(crate) automatic_filename: bool,
}

pub(crate) enum OutputProgress {
    NotStartedYet,
    InProgress {
        input_reencoding_status: InputReencodingStatus,
        outputs_finished: usize,
        output_count: usize,
    },
    Succeeded {
        output_count: usize,
    },
    Failed {
        which_failed: usize,
        error: OutputError,
    },
}

#[derive(Clone, Copy)]
pub(crate) enum InputReencodingStatus {
    Loading,
    Resizing,
    Writing,
    Reencoded,
}

#[repr(i32)]
#[derive(Default, Display)]
pub(crate) enum OutputError {
    VulkanInitializationFailed = IBLLib_Result_VulkanInitializationFailed,
    VulkanError = IBLLib_Result_VulkanError,
    InputPanoramaFileNotFound = IBLLib_Result_InputPanoramaFileNotFound,
    ShaderFileNotFound = IBLLib_Result_ShaderFileNotFound,
    ShaderCompilationFailed = IBLLib_Result_ShaderCompilationFailed,
    FileNotFound = IBLLib_Result_FileNotFound,
    InvalidArgument = IBLLib_Result_InvalidArgument,
    KtxError = IBLLib_Result_KtxError,
    StbError = IBLLib_Result_StbError,
    #[default]
    OutputCubemapPathNotValidUTF8,
    OutputLutPathNotValidUTF8,
    FailedToLoadInput(String),
    FailedToReencodeInput,
}

#[derive(Clone, Copy, Default, PartialEq, Display, Deserialize, Serialize)]
#[repr(u32)]
pub(crate) enum Distribution {
    #[default]
    Lambertian = IBLLib_Distribution_Lambertian,
    Ggx = IBLLib_Distribution_GGX,
    Charlie = IBLLib_Distribution_Charlie,
}

#[derive(Clone, Copy, Default, PartialEq, Display, Deserialize, Serialize)]
#[repr(i32)]
pub(crate) enum TargetFormat {
    R8G8B8A8Unorm = IBLLib_OutputFormat_R8G8B8A8_UNORM,
    R9G9B9E5Ufloat = IBLLib_OutputFormat_B9G9R9E5_UFLOAT,
    #[default]
    R16G16B16A16Sfloat = IBLLib_OutputFormat_R16G16B16A16_SFLOAT,
    R32G32B32A32Sfloat = IBLLib_OutputFormat_R32G32B32A32_SFLOAT,
}

struct InputImageWriter {
    temp_file: NamedTempFile,
    ok: bool,
}

impl Default for Job {
    fn default() -> Self {
        Self {
            input_path: PathBuf::new(),
            max_image_size: 4096,
            outputs: (0..DEFAULT_OUTPUT_COUNT)
                .map(Output::default_for_index)
                .collect(),
        }
    }
}

impl Output {
    pub(crate) fn default_for_index(index: usize) -> Self {
        Self {
            out_cubemap: OutputPath::new(),
            mip_level_count: None,
            cubemap_resolution: None,
            target_format: TargetFormat::R16G16B16A16Sfloat,
            lod_bias: 0.0,
            filter_settings: if index == 0 {
                None
            } else {
                Some(FilterSettings::default_for_index(index))
            },
        }
    }
}

impl FilterSettings {
    pub(crate) fn default_for_index(index: usize) -> Self {
        FilterSettings {
            distribution: match index {
                2 => Distribution::Ggx,
                3 => Distribution::Charlie,
                _ => Distribution::Lambertian,
            },
            sample_count: 1024,
            out_lut: OutputPath::new(),
        }
    }
}

impl OutputPath {
    fn new() -> OutputPath {
        OutputPath {
            path: PathBuf::new(),
            automatic_filename: true,
        }
    }
}

pub(crate) fn generate(ctx: &Context, job: Job, output_progress: Arc<Mutex<OutputProgress>>) {
    let ctx = (*ctx).clone();
    let output_count = job.outputs.len();

    thread::spawn(move || {
        let input_path = match reencode_input_image(&ctx, &job, output_count, &output_progress) {
            Ok(input_path) => input_path,
            Err(output_error) => {
                report_output_error(&ctx, &output_progress, 0, output_error);
                return;
            }
        };

        // Redirect `stdout` to a temporary file so we can capture it.
        let stdout_redirection_file = Builder::new()
            .prefix("IblStdoutLogRedirect")
            .suffix(".txt")
            .tempfile()
            .ok()
            .and_then(|temp_file| temp_file.keep().ok())
            .map(|(_, path)| path);
        if let Some(stdout_redirection_file) = stdout_redirection_file
            .as_ref()
            .and_then(|path| CString::new(path.to_str()?).ok())
        {
            unsafe {
                libc::freopen(
                    stdout_redirection_file.as_ptr(),
                    b"w\0".as_ptr() as *const c_char,
                    libc_stdhandle::stdout(),
                );
            }
        }

        for (output_index, output) in job.outputs.iter().enumerate() {
            if let Err(error) = generate_one_output(output, &input_path) {
                maybe_log_stdout_redirection_file(stdout_redirection_file);
                report_output_error(&ctx, &output_progress, output_index, error);
                return;
            }

            if output_index + 1 != output_count {
                set_output_progress(
                    &ctx,
                    &output_progress,
                    OutputProgress::InProgress {
                        input_reencoding_status: InputReencodingStatus::Reencoded,
                        outputs_finished: output_index + 1,
                        output_count,
                    },
                );
            }
        }

        maybe_log_stdout_redirection_file(stdout_redirection_file);

        set_output_progress(
            &ctx,
            &output_progress,
            OutputProgress::Succeeded { output_count },
        );
    });
}

fn reencode_input_image(
    ctx: &Context,
    job: &Job,
    output_count: usize,
    output_progress: &Mutex<OutputProgress>,
) -> Result<CString, OutputError> {
    // Load image.
    // TODO: We might be able to skip the reencoding part if this is an HDR image already.
    set_input_reencoding_status(
        ctx,
        InputReencodingStatus::Loading,
        output_count,
        output_progress,
    );
    let mut input_image = load_image(&job.input_path)
        .map_err(|error| OutputError::FailedToLoadInput(error.to_string()))?;

    // Resize the image so it fits within the user's requested bounds.
    set_input_reencoding_status(
        ctx,
        InputReencodingStatus::Resizing,
        output_count,
        output_progress,
    );
    input_image = input_image.resize(job.max_image_size, job.max_image_size, FilterType::Lanczos3);

    // Open temporary file.
    let mut input_image_writer = InputImageWriter {
        temp_file: Builder::new()
            .prefix(job.input_path.file_stem().unwrap_or(OsStr::new(".tmp")))
            .suffix(".hdr")
            .tempfile()
            .map_err(|error| OutputError::FailedToLoadInput(error.to_string()))?,
        ok: true,
    };

    // Use `stb_image_write` to write a `.hdr` image.
    set_input_reencoding_status(
        ctx,
        InputReencodingStatus::Writing,
        output_count,
        output_progress,
    );
    let input_image = input_image.to_rgba32f();
    let ok = unsafe {
        bindgen::stbi_write_hdr_to_func(
            Some(input_file_writer),
            (&mut input_image_writer) as *mut InputImageWriter as *mut c_void,
            input_image.width().try_into().unwrap(),
            input_image.height().try_into().unwrap(),
            4,
            input_image.as_flat_samples().as_slice().as_ptr(),
        )
    };
    if ok == 0 || !input_image_writer.ok {
        return Err(OutputError::FailedToReencodeInput);
    }

    drop(input_image_writer.temp_file.flush());

    // Persist that temporary file.
    let (_, input_path) = input_image_writer
        .temp_file
        .keep()
        .map_err(|error| OutputError::FailedToLoadInput(error.to_string()))?;
    let input_path = input_path
        .as_os_str()
        .to_str()
        .expect("Temporary files should be valid UTF-8");
    let input_path = CString::new(input_path).unwrap();

    set_input_reencoding_status(
        ctx,
        InputReencodingStatus::Reencoded,
        output_count,
        output_progress,
    );
    Ok(input_path)
}

fn set_input_reencoding_status(
    ctx: &Context,
    status: InputReencodingStatus,
    output_count: usize,
    output_progress: &Mutex<OutputProgress>,
) {
    set_output_progress(
        ctx,
        output_progress,
        OutputProgress::InProgress {
            input_reencoding_status: status,
            outputs_finished: 0,
            output_count,
        },
    );
}

fn generate_one_output(output: &Output, input_path: &CString) -> Result<(), OutputError> {
    let cubemap_path = output
        .out_cubemap
        .path
        .to_str()
        .and_then(|path| CString::new(path).ok())
        .ok_or(OutputError::OutputCubemapPathNotValidUTF8)?;

    let error = unsafe {
        match output.filter_settings {
            None => {
                bindgen::IBLLib_sample(
                    input_path.as_ptr(),
                    cubemap_path.as_ptr(),
                    ptr::null(),
                    IBLLib_Distribution_None,
                    output.cubemap_resolution.unwrap_or_default(),
                    output.mip_level_count.unwrap_or_default(),
                    0,
                    output.target_format as _,
                    output.lod_bias,
                    /*debugOutput=*/ true,
                )
            }
            Some(ref filter_settings) => {
                let lut_path = filter_settings
                    .out_lut
                    .path
                    .to_str()
                    .and_then(|path| CString::new(path).ok())
                    .ok_or(OutputError::OutputLutPathNotValidUTF8)?;

                bindgen::IBLLib_sample(
                    input_path.as_ptr(),
                    cubemap_path.as_ptr(),
                    lut_path.as_ptr(),
                    filter_settings.distribution as _,
                    output.cubemap_resolution.unwrap_or_default(),
                    output.mip_level_count.unwrap_or_default(),
                    filter_settings.sample_count,
                    output.target_format as _,
                    output.lod_bias,
                    /*debugOutput=*/ true,
                )
            }
        }
    };

    if error == IBLLib_Result_Success {
        Ok(())
    } else {
        Err(OutputError::from(error))
    }
}

fn report_output_error(
    ctx: &Context,
    output_progress: &Mutex<OutputProgress>,
    output_index: usize,
    output_error: OutputError,
) {
    set_output_progress(
        ctx,
        output_progress,
        OutputProgress::Failed {
            which_failed: output_index,
            error: output_error,
        },
    );
}

fn set_output_progress(
    ctx: &Context,
    output_progress_slot: &Mutex<OutputProgress>,
    output_progress: OutputProgress,
) {
    *output_progress_slot.lock().unwrap() = output_progress;
    ctx.request_repaint();
}

fn maybe_log_stdout_redirection_file(stdout_redirection_file: Option<PathBuf>) {
    // Flush the file and close it so that we can open it on the Rust side.
    unsafe {
        let stdout = libc_stdhandle::stdout();
        libc::fflush(stdout);
        libc::fclose(stdout);
    }

    // Load and log the contents of the temporary file.
    let Some(stdout_redirection_file) = stdout_redirection_file else { return };
    let Ok(file) = File::open(stdout_redirection_file) else { return };
    for line in BufReader::new(file).lines().flatten() {
        info!("{}", line);
    }
}

impl From<IBLLib_Result> for OutputError {
    fn from(value: IBLLib_Result) -> Self {
        match value {
            IBLLib_Result_VulkanInitializationFailed => OutputError::VulkanInitializationFailed,
            IBLLib_Result_VulkanError => OutputError::VulkanError,
            IBLLib_Result_InputPanoramaFileNotFound => OutputError::InputPanoramaFileNotFound,
            IBLLib_Result_ShaderFileNotFound => OutputError::ShaderFileNotFound,
            IBLLib_Result_ShaderCompilationFailed => OutputError::ShaderCompilationFailed,
            IBLLib_Result_FileNotFound => OutputError::FileNotFound,
            IBLLib_Result_KtxError => OutputError::KtxError,
            IBLLib_Result_StbError => OutputError::StbError,
            _ => OutputError::InvalidArgument,
        }
    }
}

pub(crate) fn load_image(path: &PathBuf) -> Result<DynamicImage, Error> {
    // First, try `image`.
    let mut bytes = vec![];
    File::open(path)?.read_to_end(&mut bytes)?;
    if let Ok(reader) = Reader::new(BufReader::new(Cursor::new(&bytes))).with_guessed_format() {
        if let Ok(image) = reader.decode() {
            return Ok(image);
        }
    }

    // If that fails, try `stb_image`.
    let (mut width, mut height, mut channels) = (0, 0, 0);
    let stb_pixels = unsafe {
        bindgen::stbi_load_from_memory(
            bytes.as_ptr(),
            bytes.len().try_into()?,
            &mut width,
            &mut height,
            &mut channels,
            0,
        )
    };

    if stb_pixels.is_null() {
        return Err(Error::msg(t!("input.error.failed")));
    }

    let mut pixels = vec![0; width as usize * height as usize];
    unsafe {
        ptr::copy_nonoverlapping(stb_pixels, pixels.as_mut_ptr(), pixels.len());
        bindgen::stbi_image_free(stb_pixels as *mut c_void);
    }

    let image = match channels {
        1 => DynamicImage::ImageLuma8(
            ImageBuffer::from_vec(width.try_into()?, height.try_into()?, pixels).unwrap(),
        ),
        2 => DynamicImage::ImageLumaA8(
            ImageBuffer::from_vec(width.try_into()?, height.try_into()?, pixels).unwrap(),
        ),
        3 => DynamicImage::ImageRgb8(
            ImageBuffer::from_vec(width.try_into()?, height.try_into()?, pixels).unwrap(),
        ),
        4 => DynamicImage::ImageRgba8(
            ImageBuffer::from_vec(width.try_into()?, height.try_into()?, pixels).unwrap(),
        ),
        _ => return Err(Error::msg(t!("input.error.bad.channel.count"))),
    };

    Ok(image)
}

unsafe extern "C" fn input_file_writer(userdata: *mut c_void, ptr: *mut c_void, len: c_int) {
    let input_image_writer = userdata as *mut InputImageWriter;
    if !(*input_image_writer).ok {
        return;
    }

    if (*input_image_writer)
        .temp_file
        .write_all(slice::from_raw_parts(
            ptr as *const c_void as *const u8,
            len as usize,
        ))
        .is_err()
    {
        (*input_image_writer).ok = false;
    }
}

impl ToLocalizedString for OutputError {
    fn to_localized_string(&self) -> String {
        match *self {
            OutputError::VulkanInitializationFailed => {
                t!("output.error.vulkan.initialization.failed")
            }
            OutputError::VulkanError => t!("output.error.vulkan.error"),
            OutputError::InputPanoramaFileNotFound => {
                t!("output.error.input.panorama.file.not.found")
            }
            OutputError::ShaderFileNotFound => t!("output.error.shader.file.not.found"),
            OutputError::ShaderCompilationFailed => t!("output.error.shader.compilation.failed"),
            OutputError::FileNotFound => t!("output.error.file.not.found"),
            OutputError::InvalidArgument => t!("output.error.invalid.argument"),
            OutputError::KtxError => t!("output.error.ktx.error"),
            OutputError::StbError => t!("output.error.stb.error"),
            OutputError::OutputCubemapPathNotValidUTF8 => {
                t!("output.error.output.cubemap.path.not.valid.utf8")
            }
            OutputError::OutputLutPathNotValidUTF8 => {
                t!("output.error.output.lut.path.not.valid.utf8")
            }
            OutputError::FailedToLoadInput(ref error) => {
                t!("output.error.failed.to.load.input", error = error)
            }
            OutputError::FailedToReencodeInput => t!("output.error.failed.to.reencode.input"),
        }
    }
}

impl ToLocalizedString for Option<Distribution> {
    fn to_localized_string(&self) -> String {
        match *self {
            None => t!("output.distribution.none"),
            Some(Distribution::Lambertian) => t!("output.distribution.lambertian"),
            Some(Distribution::Ggx) => t!("output.distribution.ggx"),
            Some(Distribution::Charlie) => t!("output.distribution.charlie"),
        }
    }
}

impl ToLocalizedString for TargetFormat {
    fn to_localized_string(&self) -> String {
        match *self {
            TargetFormat::R8G8B8A8Unorm => t!("output.target.format.8"),
            TargetFormat::R9G9B9E5Ufloat => t!("output.target.format.9995"),
            TargetFormat::R16G16B16A16Sfloat => t!("output.target.format.16"),
            TargetFormat::R32G32B32A32Sfloat => t!("output.target.format.32"),
        }
    }
}
