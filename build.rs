// gltf-ibl-sampler-egui/build.rs

use bindgen::{Builder, CargoCallbacks};
use cc::Build;
use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Build the glTF IBL Sampler.
    let gltf_ibl_sampler_dir = Path::new("glTF-IBL-Sampler/").to_owned();
    let gltf_ibl_sampler_include_dir = gltf_ibl_sampler_dir.join("lib/include");
    let gltf_ibl_sampler_src: Vec<_> = [
        "FileHelper.cpp",
        "format.cpp",
        "ktxImage.cpp",
        "lib.cpp",
        "STBImage.cpp",
        "vkHelper.cpp",
    ]
    .into_iter()
    .map(|filename| gltf_ibl_sampler_dir.join("lib/source").join(filename))
    .collect();
    Build::new()
        .files(&gltf_ibl_sampler_src)
        .file(gltf_ibl_sampler_dir.join("thirdparty/volk/volk.c"))
        .include(&gltf_ibl_sampler_include_dir)
        .include(gltf_ibl_sampler_dir.join("thirdparty/stb"))
        .include(gltf_ibl_sampler_dir.join("thirdparty/volk"))
        .include(gltf_ibl_sampler_dir.join("thirdparty/Vulkan-Headers/include"))
        .cpp(true)
        .std("c++17")
        .compile("IBLLib");
    for source_file in &gltf_ibl_sampler_src {
        // FIXME: This should include headers too.
        println!("cargo:rerun-if-changed={}", source_file.display());
    }

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
        gltf_ibl_sampler_include_dir.to_string_lossy().into_owned(),
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
}
