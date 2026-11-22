//! Precompiles the HLSL shaders to DXBC.
//!
//! Compiling every entry point with FXC takes roughly 180 ms, almost all of it
//! the quad pixel shader, whose analytic shadow integral the compiler unrolls.
//! That is too long to pay on every window open, so when the build host is
//! Windows the bytecode is produced here and embedded with `include_bytes!`.
//!
//! `D3DCompile` is called through a `raw-dylib` import of `d3dcompiler_47.dll`
//! (part of Windows) rather than through the `windows` crate, which keeps the
//! build script free of dependencies. When the host is not Windows — a cross
//! build — nothing is precompiled and the crate falls back to compiling at
//! runtime, which is correct, only slower to start.

use std::path::{Path, PathBuf};

include!("src/shader_table.rs");

fn main() {
    println!("cargo::rustc-check-cfg=cfg(spherekit_precompiled_shaders)");
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=src/shader_table.rs");
    println!("cargo::rerun-if-changed=shaders");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    if precompile(&manifest.join("shaders"), &out_dir) {
        println!("cargo::rustc-cfg=spherekit_precompiled_shaders");
    }
}

#[cfg(windows)]
fn precompile(shader_dir: &Path, out_dir: &Path) -> bool {
    let read = |name: &str| {
        let path = shader_dir.join(name);
        println!("cargo::rerun-if-changed={}", path.display());
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
    };
    let common = read("common.hlsl");

    let mut index = String::from(
        "/// Precompiled DXBC for every entry point, in `SHADER_TABLE` order.\n\
         static PRECOMPILED: [&[u8]; 13] = [\n",
    );
    for (stem, file, function, profile) in SHADER_TABLE {
        let source = compose_shader_source(&common, file, &read(file));
        let code = match fxc::compile(&source, file, function, profile) {
            Ok(code) => code,
            Err(message) => panic!("HLSL {file}:{function} failed to compile:\n{message}"),
        };
        let path = out_dir.join(format!("{stem}.dxbc"));
        std::fs::write(&path, code).expect("writing precompiled shader");
        index.push_str(&format!("    include_bytes!({:?}),\n", path.display().to_string()));
    }
    index.push_str("];\n");
    std::fs::write(out_dir.join("precompiled.rs"), index).expect("writing precompiled index");
    true
}

#[cfg(not(windows))]
fn precompile(_shader_dir: &Path, _out_dir: &Path) -> bool {
    false
}

#[cfg(windows)]
mod fxc {
    use core::ffi::c_void;

    #[link(name = "d3dcompiler_47", kind = "raw-dylib")]
    unsafe extern "system" {
        fn D3DCompile(
            src: *const c_void,
            len: usize,
            source_name: *const u8,
            defines: *const c_void,
            include: *const c_void,
            entry: *const u8,
            target: *const u8,
            flags1: u32,
            flags2: u32,
            code: *mut *mut c_void,
            errors: *mut *mut c_void,
        ) -> i32;
    }

    /// The leading slots of `ID3DBlob`'s vtable.
    #[repr(C)]
    struct BlobVtbl {
        query_interface: usize,
        add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        get_buffer_pointer: unsafe extern "system" fn(*mut c_void) -> *mut c_void,
        get_buffer_size: unsafe extern "system" fn(*mut c_void) -> usize,
    }

    /// Copies a blob's bytes out and releases it.
    ///
    /// # Safety
    ///
    /// `blob` must be null or a live `ID3DBlob` the caller owns a reference to.
    unsafe fn take_blob(blob: *mut c_void) -> Vec<u8> {
        if blob.is_null() {
            return Vec::new();
        }
        // SAFETY: per the contract, `blob` is a COM object whose first field is
        // its vtable pointer, laid out as `BlobVtbl` describes.
        unsafe {
            let vtbl = &**(blob as *const *const BlobVtbl);
            let ptr = (vtbl.get_buffer_pointer)(blob) as *const u8;
            let len = (vtbl.get_buffer_size)(blob);
            let bytes = std::slice::from_raw_parts(ptr, len).to_vec();
            (vtbl.release)(blob);
            bytes
        }
    }

    fn c_string(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    pub fn compile(
        source: &str,
        file: &str,
        function: &str,
        profile: &str,
    ) -> Result<Vec<u8>, String> {
        let (name, entry, target) = (c_string(file), c_string(function), c_string(profile));
        let mut code = core::ptr::null_mut();
        let mut errors = core::ptr::null_mut();
        // SAFETY: every pointer is valid for the call; the out-parameters are
        // released by `take_blob`.
        let hr = unsafe {
            D3DCompile(
                source.as_ptr().cast(),
                source.len(),
                name.as_ptr(),
                core::ptr::null(),
                core::ptr::null(),
                entry.as_ptr(),
                target.as_ptr(),
                super::SHADER_COMPILE_FLAGS,
                0,
                &mut code,
                &mut errors,
            )
        };
        // SAFETY: both are null or blobs the compiler handed us.
        let (code, errors) = unsafe { (take_blob(code), take_blob(errors)) };
        if hr < 0 || code.is_empty() {
            let message = String::from_utf8_lossy(&errors).trim_end_matches('\0').to_string();
            return Err(format!("HRESULT {hr:#010x}: {message}"));
        }
        Ok(code)
    }
}
