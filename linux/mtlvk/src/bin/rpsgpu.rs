//! x86Sim: make a render pipeline state mtlvk refused (var/metal/failed/<id>-{vert,frag}.metallib) on this host's GPU
//! through metal2vulkan-runtime, as RPS_NEW does. Usage:
//!   rpsgpu <vert.metallib> <frag.metallib|-> [I:FORMAT_RAW[+blend] ...] [samples=N] [depth=F] [stencil=F]
use metal2vulkan::library::Library;
use metal2vulkan::metal::PixelFormat;
use metal2vulkan::pipeline::{ColorAttachmentDescriptor, FunctionDescriptor, RenderPipelineDescriptor, StageSource};
use metal2vulkan_runtime::{Device, DeviceOptions, Instance};

fn func(path: &str) -> FunctionDescriptor {
    let lib = Library::parse(&std::fs::read(path).expect("read")).expect("parse");
    let name = lib.functions().next().expect("function").name.to_string();
    FunctionDescriptor::new(lib.function(&name).expect("function"))
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let instance = Instance::new(&[]).expect("instance");
    let physical = instance.physical_devices().expect("physical").into_iter().find(|p| p.check().is_ok()).expect("device");
    let device = Device::new(&physical, DeviceOptions::default()).expect("device");
    let mut d = RenderPipelineDescriptor::new(StageSource::Function(func(&a[0])));
    if a[1] != "-" {
        d.fragment = Some(StageSource::Function(func(&a[1])));
    }
    for s in &a[2..] {
        if let Some(n) = s.strip_prefix("samples=") {
            d.raster_sample_count = n.parse().unwrap();
        } else if let Some(n) = s.strip_prefix("stencil=") {
            d.stencil_format = Some(PixelFormat::try_from(n.parse::<u32>().unwrap()).unwrap());
        } else if let Some(n) = s.strip_prefix("depth=") {
            d.depth_format = Some(PixelFormat::try_from(n.parse::<u32>().unwrap()).unwrap());
        } else {
            let (i, f) = s.split_once(':').unwrap();
            let (f, blend) = match f.strip_suffix("+blend") { Some(f) => (f, true), None => (f, false) };
            let mut c = ColorAttachmentDescriptor::new(PixelFormat::try_from(f.parse::<u32>().unwrap()).unwrap());
            c.blending_enabled = blend;
            d.color_attachments[i.parse::<usize>().unwrap()] = Some(c);
        }
    }
    match device.new_render_pipeline_state(&d, |_| Ok(vec![])) {
        Ok(_) => println!("OK"),
        Err(e) => println!("ERR {e}\n{e:?}"),
    }
}
