// SPDX-License-Identifier: GPL-3.0-or-later

pub(crate) fn timestamp_features(backend: wgpu::Backend, available: wgpu::Features) -> wgpu::Features {
    // Metal counter allocation failure marks the whole wgpu device as lost.
    // Optional timing must not make stabilization depend on counter resources.
    if backend == wgpu::Backend::Metal {
        wgpu::Features::empty()
    } else {
        available & wgpu::Features::TIMESTAMP_QUERY
    }
}

pub(crate) struct BufferUnmapGuard<'a>(pub(crate) &'a wgpu::Buffer);

impl Drop for BufferUnmapGuard<'_> {
    fn drop(&mut self) {
        // wgpu records the mapped range before checking whether mapping succeeds.
        // Reset it after failures too, before the buffer can be used again.
        self.0.unmap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> (wgpu::Device, wgpu::Queue) {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .expect("a GPU adapter is required for the readback regression");
        eprintln!("readback regression adapter: {:?}", adapter.get_info());
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("GPU device creation failed");
        device.on_uncaptured_error(std::sync::Arc::new(|error| eprintln!("injected GPU error: {error}")));
        (device, queue)
    }

    fn map_result(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Result<(), wgpu::BufferAsyncError> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        receiver.recv_timeout(std::time::Duration::from_secs(5)).expect("mapping callback must complete")
    }

    fn readback_buffer(device: &wgpu::Device) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback regression"), size: 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        })
    }

    #[test]
    fn metal_timing_does_not_request_counter_resources() {
        assert!(timestamp_features(wgpu::Backend::Metal, wgpu::Features::all()).is_empty());
        for backend in [wgpu::Backend::Vulkan, wgpu::Backend::Dx12] {
            assert_eq!(timestamp_features(backend, wgpu::Features::all()), wgpu::Features::TIMESTAMP_QUERY);
            assert!(timestamp_features(backend, wgpu::Features::empty()).is_empty());
        }
    }

    #[test]
    fn failed_mapping_without_cleanup_reproduces_the_reported_panic() {
        let (device, _) = device();
        let buffer = readback_buffer(&device);
        buffer.destroy();
        assert!(map_result(&device, &buffer).is_err());
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        })).expect_err("the old failure path leaves the buffer mapped");
        let message = panic.downcast_ref::<String>().map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied()).unwrap_or_default();
        assert!(message.contains("Buffer is already mapped"), "unexpected panic: {message}");
        buffer.unmap();
    }

    #[test]
    fn failed_mapping_can_be_retried_without_panicking() {
        let (device, _) = device();
        let buffer = readback_buffer(&device);
        buffer.destroy();
        for _ in 0..3 {
            let _unmap = BufferUnmapGuard(&buffer);
            assert!(map_result(&device, &buffer).is_err());
        }
        let valid = readback_buffer(&device);
        let _unmap = BufferUnmapGuard(&valid);
        assert!(map_result(&device, &valid).is_ok());
    }

    #[test]
    fn lost_device_mapping_can_fail_repeatedly_without_panicking() {
        let (device, _) = device();
        let buffer = readback_buffer(&device);
        device.destroy();
        for _ in 0..3 {
            let _unmap = BufferUnmapGuard(&buffer);
            assert!(map_result(&device, &buffer).is_err());
        }
    }

    #[test]
    fn successful_readbacks_preserve_bytes_and_release_the_mapping() {
        let (device, queue) = device();
        let source = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback source"), size: 16,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let buffer = readback_buffer(&device);
        for value in [17u8, 92, 203] {
            queue.write_buffer(&source, 0, &[value; 16]);
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            encoder.copy_buffer_to_buffer(&source, 0, &buffer, 0, 16);
            queue.submit([encoder.finish()]);
            let _unmap = BufferUnmapGuard(&buffer);
            assert!(map_result(&device, &buffer).is_ok());
            let view = buffer.slice(..).get_mapped_range();
            assert_eq!(&*view, &[value; 16]);
        }
    }
}
