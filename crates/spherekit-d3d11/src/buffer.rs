//! Growable CPU-written GPU buffers.
//!
//! Every per-frame buffer is `D3D11_USAGE_DYNAMIC` and written with
//! `Map(WRITE_DISCARD)`. Discard hands back fresh memory while the GPU may
//! still be reading last frame's contents, so there is no stall and no need to
//! ring-buffer by hand: the driver renames the allocation behind the scenes.
//!
//! Buffers grow to the next power of two and never shrink. A UI's frame size
//! is stable, so after the first few frames nothing reallocates.

use spherekit_core::RenderError;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_INDEX_BUFFER, D3D11_BIND_SHADER_RESOURCE,
    D3D11_BIND_VERTEX_BUFFER, D3D11_BUFFER_DESC, D3D11_CPU_ACCESS_WRITE, D3D11_MAP_WRITE_DISCARD,
    D3D11_MAPPED_SUBRESOURCE, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, D3D11_USAGE_DYNAMIC,
    ID3D11Buffer, ID3D11Device, ID3D11DeviceContext, ID3D11ShaderResourceView,
};

/// What a buffer is bound as.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum BufferKind {
    /// Per-vertex or per-instance attributes.
    Vertex,
    /// 32-bit indices.
    Index,
    /// A `StructuredBuffer<T>` table with the given element stride.
    Structured(u32),
    /// A constant buffer of fixed size.
    Constant,
}

/// One dynamic buffer that grows on demand.
pub(crate) struct DynamicBuffer {
    kind: BufferKind,
    buffer: Option<ID3D11Buffer>,
    srv: Option<ID3D11ShaderResourceView>,
    capacity: u64,
    min_capacity: u64,
}

impl DynamicBuffer {
    /// A buffer that allocates on first write, at least `min_capacity` bytes.
    pub fn new(kind: BufferKind, min_capacity: u64) -> Self {
        Self { kind, buffer: None, srv: None, capacity: 0, min_capacity }
    }

    /// The buffer, once it has been written at least once.
    #[inline]
    pub fn buffer(&self) -> Option<&ID3D11Buffer> {
        self.buffer.as_ref()
    }

    /// The shader resource view of a structured buffer.
    #[inline]
    pub fn srv(&self) -> Option<&ID3D11ShaderResourceView> {
        self.srv.as_ref()
    }

    /// Allocated size in bytes.
    #[inline]
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Replaces the buffer's contents, growing it first when it is too small.
    ///
    /// Returns `true` when the buffer was reallocated, which invalidates any
    /// binding of the old one.
    pub fn write(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        bytes: &[u8],
    ) -> Result<bool, RenderError> {
        let needed = (bytes.len() as u64).max(1);
        let mut reallocated = false;
        if self.buffer.is_none() || needed > self.capacity {
            let capacity = match self.kind {
                // A constant buffer is exactly the size of its struct.
                BufferKind::Constant => needed.next_multiple_of(16),
                _ => needed.max(self.min_capacity).next_power_of_two(),
            };
            self.allocate(device, capacity)?;
            reallocated = true;
        }
        if bytes.is_empty() {
            return Ok(reallocated);
        }
        let buffer = self.buffer.as_ref().expect("allocated above");
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the buffer is dynamic and CPU-writable; `Map` returns at least
        // `capacity` writable bytes, and `bytes.len() <= capacity`.
        unsafe {
            context
                .Map(buffer, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))
                .map_err(|e| RenderError::Backend(format!("mapping a buffer failed: {e}")))?;
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.pData.cast::<u8>(), bytes.len());
            context.Unmap(buffer, 0);
        }
        Ok(reallocated)
    }

    fn allocate(&mut self, device: &ID3D11Device, capacity: u64) -> Result<(), RenderError> {
        let (bind, misc, stride) = match self.kind {
            BufferKind::Vertex => (D3D11_BIND_VERTEX_BUFFER.0, 0, 0),
            BufferKind::Index => (D3D11_BIND_INDEX_BUFFER.0, 0, 0),
            BufferKind::Structured(stride) => {
                (D3D11_BIND_SHADER_RESOURCE.0, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED.0, stride)
            }
            BufferKind::Constant => (D3D11_BIND_CONSTANT_BUFFER.0, 0, 0),
        };
        let byte_width = u32::try_from(capacity)
            .map_err(|_| RenderError::OutOfMemory { what: "GPU buffer", bytes: capacity })?;
        // A structured buffer's size must be a whole number of elements.
        let byte_width = if stride > 0 { byte_width.next_multiple_of(stride) } else { byte_width };
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: byte_width,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: bind as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            MiscFlags: misc as u32,
            StructureByteStride: stride,
        };
        let mut buffer = None;
        // SAFETY: `desc` is a valid descriptor and the out-pointer a local.
        unsafe { device.CreateBuffer(&desc, None, Some(&mut buffer)) }
            .map_err(|_| RenderError::OutOfMemory { what: "GPU buffer", bytes: capacity })?;
        let buffer = buffer.expect("CreateBuffer succeeded");
        self.srv = if stride > 0 {
            let mut srv = None;
            // SAFETY: a null descriptor views the whole structured buffer.
            unsafe { device.CreateShaderResourceView(&buffer, None, Some(&mut srv)) }
                .map_err(|e| RenderError::Backend(format!("structured buffer view: {e}")))?;
            srv
        } else {
            None
        };
        self.buffer = Some(buffer);
        self.capacity = byte_width as u64;
        Ok(())
    }
}
