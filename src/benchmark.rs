use crate::platform;

pub async fn benchmark_gpu(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<f64> {
    let source = match platform::fs::read_to_string("shaders/gpu_bench.wgsl") {
        Ok(s) => s,
        Err(e) => {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::warn_1(&format!("[molasses] gpu benchmark shader not found: {e}").into());
            return None;
        }
    };
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gpu_bench_module"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Owned(source)),
    });

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_bench_buffer"),
        size: 1024 * 4,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("gpu_bench_bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("gpu_bench_layout"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("gpu_bench_pipeline"),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("gpu_bench_bg"),
        layout: &bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    });

    let start = platform::Instant::now();

    // Timed dispatch (includes first-run shader compilation + compute + copy).
    {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            cpass.set_pipeline(&pipeline);
            cpass.set_bind_group(0, &bind_group, &[]);
            cpass.dispatch_workgroups(16, 1, 1);
        }
        queue.submit(Some(encoder.finish()));
    }

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_bench_staging"),
        size: buffer.size(),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(&buffer, 0, &staging, 0, buffer.size());
        queue.submit(Some(encoder.finish()));
    }

    let (sender, receiver) = std::sync::mpsc::channel();
    let slice = staging.slice(..);
    slice.map_async(wgpu::MapMode::Read, move |_| {
        let _ = sender.send(());
    });

    // Poll until the map callback fires.
    #[cfg(not(target_family = "wasm"))]
    loop {
        let _ = device.poll(wgpu::PollType::Poll);
        if receiver.try_recv().is_ok() {
            break;
        }
        std::hint::spin_loop();
    }

    #[cfg(target_family = "wasm")]
    loop {
        let _ = device.poll(wgpu::PollType::Poll);
        if receiver.try_recv().is_ok() {
            break;
        }
        wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&wasm_bindgen::JsValue::NULL))
            .await
            .unwrap();
    }

    staging.unmap();

    Some(start.elapsed().as_secs_f64() * 1000.0)
}
