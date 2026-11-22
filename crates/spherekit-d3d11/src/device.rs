//! Device creation and adapter description.

use spherekit_core::InitError;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_DRIVER_TYPE_WARP,
    D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_DEBUG, D3D11_CREATE_DEVICE_FLAG,
    D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_CREATE_FACTORY_FLAGS,
    DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory2,
    IDXGIFactory6,
};
use windows::core::Interface;

/// The feature levels requested, best first.
///
/// 11_0 is the floor, not 10_0. The shaders are written against shader model
/// 5 and read their transform, clip and gradient tables as `StructuredBuffer`s,
/// which 10.x hardware only exposes to compute shaders. Supporting it would
/// mean a second, typed-buffer copy of every shader for GPUs from before 2010;
/// those machines get the Skia backend instead, which the caller falls back to
/// on any construction error.
const FEATURE_LEVELS: [D3D_FEATURE_LEVEL; 2] = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];

/// Which kind of device to accept.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum DeviceKind {
    /// A hardware adapter only.
    ///
    /// Windowed rendering never falls back to WARP: a software rasteriser
    /// driving a swap chain is slower than the Skia CPU backend, which the
    /// caller falls back to when this fails.
    Hardware,
    /// Hardware if there is any, WARP otherwise. For offscreen use and tests,
    /// where a deterministic software device is better than none.
    HardwareOrWarp,
}

/// A device, its immediate context, and what to call it.
pub(crate) struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    /// `"Direct3D 11 (<adapter>, FL 11_1)"`.
    pub adapter_name: String,
}

impl Gpu {
    /// Creates a device of the requested kind.
    pub fn create(kind: DeviceKind) -> Result<Self, InitError> {
        // The high-performance adapter first, matching the wgpu backend's
        // `PowerPreference::HighPerformance`: on a hybrid laptop the default
        // adapter is the integrated one.
        let preferred = preferred_adapter();
        let attempt = match &preferred {
            Some(adapter) => create_device(Some(adapter), D3D_DRIVER_TYPE_UNKNOWN)
                .or_else(|_| create_device(None, D3D_DRIVER_TYPE_HARDWARE)),
            None => create_device(None, D3D_DRIVER_TYPE_HARDWARE),
        };
        let (device, context, feature_level) = match (attempt, kind) {
            (Ok(created), _) => created,
            (Err(e), DeviceKind::Hardware) => {
                return Err(InitError::NoAdapter(format!(
                    "no Direct3D 11 hardware device at feature level 11_0 or above: {e}"
                )));
            }
            (Err(hw), DeviceKind::HardwareOrWarp) => create_device(None, D3D_DRIVER_TYPE_WARP)
                .map_err(|warp| {
                    InitError::NoAdapter(format!(
                        "no Direct3D 11 device: hardware failed ({hw}), WARP failed ({warp})"
                    ))
                })?,
        };

        let adapter = adapter_description(&device).unwrap_or_else(|| "unknown adapter".into());
        let adapter_name =
            format!("Direct3D 11 ({adapter}, FL {})", feature_level_name(feature_level));
        tracing::info!(target: "spherekit_d3d11", adapter = %adapter_name, "device created");
        Ok(Self { device, context, adapter_name })
    }

    /// The DXGI factory that owns this device's adapter.
    ///
    /// A swap chain must be created by the factory of the adapter its device
    /// runs on, which is not necessarily a freshly created one.
    pub fn factory(&self) -> windows::core::Result<IDXGIFactory2> {
        // SAFETY: plain COM calls on live interfaces.
        unsafe {
            let dxgi: IDXGIDevice = self.device.cast()?;
            let adapter: IDXGIAdapter = dxgi.GetAdapter()?;
            adapter.GetParent()
        }
    }
}

fn preferred_adapter() -> Option<IDXGIAdapter1> {
    // SAFETY: plain COM calls; every out-value is owned by the returned
    // interface.
    unsafe {
        let factory: IDXGIFactory6 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).ok()?;
        let adapter: IDXGIAdapter1 =
            factory.EnumAdapterByGpuPreference(0, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE).ok()?;
        let desc = adapter.GetDesc1().ok()?;
        // The Basic Render Driver enumerates as an adapter too; it is WARP by
        // another name and must not satisfy a hardware-only request.
        ((desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) == 0).then_some(adapter)
    }
}

fn create_device(
    adapter: Option<&IDXGIAdapter1>,
    driver: D3D_DRIVER_TYPE,
) -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext, D3D_FEATURE_LEVEL)> {
    // BGRA support is what lets the device share surfaces with DirectComposition
    // and Direct2D; it costs nothing when unused.
    let base = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
    let with_debug = |flags: D3D11_CREATE_DEVICE_FLAG| {
        if cfg!(debug_assertions) { flags | D3D11_CREATE_DEVICE_DEBUG } else { flags }
    };
    let attempt = |flags: D3D11_CREATE_DEVICE_FLAG| {
        let mut device = None;
        let mut context = None;
        let mut level = D3D_FEATURE_LEVEL::default();
        let adapter: Option<IDXGIAdapter> = adapter.and_then(|a| a.cast().ok());
        // SAFETY: the out-pointers are locals that outlive the call.
        unsafe {
            D3D11CreateDevice(
                adapter.as_ref(),
                driver,
                HMODULE::default(),
                flags,
                Some(&FEATURE_LEVELS),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut level),
                Some(&mut context),
            )?;
        }
        match (device, context) {
            (Some(d), Some(c)) => Ok((d, c, level)),
            _ => Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL)),
        }
    };
    // The debug layer is an optional Windows feature ("Graphics Tools"); a
    // debug build on a machine without it must still get a device.
    attempt(with_debug(base)).or_else(|_| attempt(base))
}

fn adapter_description(device: &ID3D11Device) -> Option<String> {
    // SAFETY: plain COM calls on live interfaces.
    unsafe {
        let dxgi: IDXGIDevice = device.cast().ok()?;
        let adapter = dxgi.GetAdapter().ok()?;
        let desc = adapter.GetDesc().ok()?;
        let len = desc.Description.iter().position(|c| *c == 0).unwrap_or(desc.Description.len());
        Some(String::from_utf16_lossy(&desc.Description[..len]).trim().to_string())
    }
}

/// `"11_1"` for `D3D_FEATURE_LEVEL_11_1`, and so on.
pub(crate) fn feature_level_name(level: D3D_FEATURE_LEVEL) -> String {
    let v = level.0 as u32;
    format!("{}_{}", (v >> 12) & 0xF, (v >> 8) & 0xF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_levels_are_named_like_the_sdk_does() {
        assert_eq!(feature_level_name(D3D_FEATURE_LEVEL_11_1), "11_1");
        assert_eq!(feature_level_name(D3D_FEATURE_LEVEL_11_0), "11_0");
    }
}
