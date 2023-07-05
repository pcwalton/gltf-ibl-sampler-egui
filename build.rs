// gltf-ibl-sampler-egui/build.rs

use bindgen::{Builder, CargoCallbacks};
use cc::Build;
use cmake;
use libloading;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

// https://github.com/rust-lang/cargo/issues/1759#issuecomment-851071145
fn get_output_path() -> PathBuf {
    // <root or manifest path>/target/<profile>/
    let manifest_dir_string = env::var("CARGO_MANIFEST_DIR").unwrap();
    let build_type = env::var("PROFILE").unwrap();
    let path = Path::new(&manifest_dir_string)
        .join("target")
        .join(build_type);
    return PathBuf::from(path);
}

fn main() {
    // Build the glTF IBL sampler.
    let ibl_sampler_dest = cmake::build("glTF-IBL-Sampler");

    // Link to the glTF IBL sampler.
    println!(
        "cargo:rustc-link-search=native={}",
        ibl_sampler_dest.join("lib").display()
    );
    println!("cargo:rustc-link-lib=dylib=GltfIblSampler");
    println!("cargo:rerun-if-changed=wrapper.h");

    // Build stb_image` and `stb_image_write`.
    //
    // Yes, the glTF IBL sampler comes with this, but vendoring it ourselves avoids potential
    // breakage in the future.
    //
    // We turn off strict aliasing because the `stb_image_write` header tells us to.
    Build::new()
        .file("stb_image.c")
        .file("stb_image_write.c")
        .flag_if_supported("-fno-strict-aliasing")
        .compile("stb_image");
    println!("cargo:rerun-if-changed=stb_image.c");
    println!("cargo:rerun-if-changed=stb_image.h");
    println!("cargo:rerun-if-changed=stb_image_write.c");
    println!("cargo:rerun-if-changed=stb_image_write.h");

    // Generate Rust bindings for both the glTF IBL sampler and `stb_image_write`.
    let clang_args = vec![
            "-x".to_owned(),
            "c++".to_owned(),
            "-iquote".to_owned(),
            ibl_sampler_dest.join("include").display().to_string(),
        ];
    let bindings = Builder::default()
        .clang_args(clang_args)
        .header("wrapper.h")
        .allowlist_function("IBLLib.*")
        .allowlist_function("stbi_write_hdr_to_func")
        .allowlist_function("stbi_load_from_memory")
        .allowlist_function("stbi_image_free")
        .parse_callbacks(Box::new(CargoCallbacks))
        .generate()
        .expect("Failed to generate bindings!");

    // Write those bindings out.
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Failed to write bindings!");

    // Copy needed DLLs alongside the `.exe` we'll generate.
    //
    // You might think we could avoid this by compiling `GltfIblSampler.dll` as a static library
    // instead of a shared one, but that doesn't work because we still need `ktx.dll`, and that's
    // always a shared library.
    //
    // TODO: Non-Windows platforms.
    let ibl_sampler_dll = libloading::library_filename("GltfIblSampler");
    let ktx_dll = libloading::library_filename("ktx");
    let target_path = get_output_path();
    fs::copy(
        ibl_sampler_dest
            .join("build")
            .join("Ktx")
            .join("bin")
            .join(&ktx_dll),
        target_path.join(&ktx_dll),
    )
    .expect("Failed to copy KTX DLL");
    fs::copy(
        ibl_sampler_dest.join("bin").join(&ibl_sampler_dll),
        target_path.join(&ibl_sampler_dll),
    )
    .expect("Failed to copy IBL sampler DLL");
}
