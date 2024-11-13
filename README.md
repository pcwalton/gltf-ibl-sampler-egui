# glTF IBL Sampler UI

## Overview

This is an artist-friendly user interface that wraps the [glTF IBL Sampler] to
generate cubemap skyboxes from panoramas. It provides an easy way to generate
skyboxes for use in [Bevy] and other new game engines that use the modern
[KTX2] format as their native texture format. By default, the panorama is split
up into base color, diffuse, and specular parts, with the mipmap levels
corresponding to different roughness values of the material.

For the most part, using this tool is as easy as starting the app, dragging a
panorama in `.exr` or `.hdr` format with an equirectangular projection into the
window, and clicking Generate.

![Screenshot](https://github.com/pcwalton/gltf-ibl-sampler-egui/blob/master/etc/Screenshot.png?raw=true)

## Detailed description

This tool's user interface is built on [`egui`].

All options are automatically set to reasonable default values, but they can be
fully customized as you wish. To get a detailed description of any option,
simply hover over it with the mouse.

In general, this program simply wraps the upstream [glTF IBL Sampler], with two
notable feature additions for the sake of convenience:

1. OpenEXR `.exr` files are supported in addition to the Radiance `.hdr`
format.

2. The tool can generate unfiltered base-color skyboxes for rendering in
addition to diffuse and specular environment maps. This means that you can use
this tool as an all-in-one skybox generator for engines like [Bevy].

## Building

This repository contains submodules, so make sure to either clone it
with `git clone --recursive` or use
`git submodule init && git submodule sync && git submodule update`
after checking it out.

As the glTF IBL Sampler is a C++ app instead of a pure Rust one, you'll need
a C++ compiler such as Xcode or Visual Studio to be installed in order to
build this package. Note that the Vulkan SDK and CMake are no longer required.

Note that the skybox sampling process is itself hardware-accelerated using
Vulkan. So you'll need a Vulkan-capable GPU to usefully run this application.
This unfortunately also means that the baking process is subject to hardware
memory limitations, so baking an entire 8K × 4K panoramic texture may not
work. To avoid spurious failures stemming from this limitation, textures are
resized to at most 4K pixels on each side by default.

You should be able to run the app using `cargo run --release`.

### Windows
If compilation fails with "'stdio.h' file not found", try running in `Developer Command Prompt for VS 2022/2019` or `Developer PowerShell for VS`.

## Supported image formats

The panorama can be stored either in any format that the Rust [`image`] crate
supports, which notably includes `.exr`, or in `.hdr` format. The resulting
textures can be stored in KTX2 format, while the BRDF lookup tables are stored in PNG format.

## License

Licensed under the MIT license or the Apache 2.0 license, at your option. See
the `LICENSE-APACHE` or `LICENSE-MIT` files for details.

## Code of conduct

The glTF IBL Sampler UI follows the same Code of Conduct as Rust itself.
Reports can be made to the project authors.

[glTF IBL Sampler]: https://github.com/KhronosGroup/glTF-IBL-Sampler

[Bevy]: https://bevyengine.org/

[KTX2]: https://registry.khronos.org/KTX/specs/2.0/ktxspec.v2.html

[`egui`]: https://www.egui.rs/

[Vulkan SDK]: https://vulkan.lunarg.com/

[CMake]: https://cmake.org/

[`image`]: https://docs.rs/image/latest/image/
