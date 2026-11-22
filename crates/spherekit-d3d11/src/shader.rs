//! HLSL sources and their compilation.
//!
//! The sources live in this crate's `shaders/` directory and are embedded with
//! `include_str!`. Each shader body is prefixed with `common.hlsl` (followed by
//! a `#line` directive, so a diagnostic names the right file and line) and
//! compiled with `D3DCompile` from `d3dcompiler_47.dll`, which ships with every
//! supported version of Windows.
//!
//! ## Where compilation happens
//!
//! Compiling all thirteen entry points takes about 180 ms with FXC, nearly all
//! of it the quad pixel shader, whose eight-slice shadow integral the compiler
//! unrolls. That is too much to pay every time a window opens, so `build.rs`
//! compiles everything when the build host is Windows and the renderer loads
//! the embedded bytecode ([`load`]). A cross build from another host has no
//! FXC, so there [`load`] falls back to [`compile_all`] at device creation.
//!
//! [`compile_all`] stays public either way: it is how the tests prove that
//! every source compiles from scratch, and how its cost is measured.

use spherekit_core::ShaderError;
use std::borrow::Cow;
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::core::PCSTR;

include!("shader_table.rs");

#[cfg(spherekit_precompiled_shaders)]
include!(concat!(env!("OUT_DIR"), "/precompiled.rs"));

/// The shared prelude, prepended to every shader.
const COMMON: &str = include_str!("../shaders/common.hlsl");

/// Every shader file, as `(file name, source)`.
pub const SOURCES: &[(&str, &str)] = &[
    ("common.hlsl", COMMON),
    ("quad.hlsl", include_str!("../shaders/quad.hlsl")),
    ("text.hlsl", include_str!("../shaders/text.hlsl")),
    ("mesh.hlsl", include_str!("../shaders/mesh.hlsl")),
    ("composite.hlsl", include_str!("../shaders/composite.hlsl")),
    ("blur.hlsl", include_str!("../shaders/blur.hlsl")),
];

/// Identifies one compiled entry point.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Entry {
    /// `quad.hlsl` vertex stage, shared by solid and textured quads.
    QuadVs,
    /// `quad.hlsl` pixel stage.
    QuadPs,
    /// `text.hlsl` vertex stage, shared by both text pipelines.
    TextVs,
    /// `text.hlsl` grayscale pixel stage.
    TextPs,
    /// `text.hlsl` dual-source subpixel pixel stage.
    TextSubpixelPs,
    /// `mesh.hlsl` vertex stage.
    MeshVs,
    /// `mesh.hlsl` vertex-colour pixel stage.
    MeshPs,
    /// `mesh.hlsl` textured pixel stage.
    MeshTexturedPs,
    /// `composite.hlsl` vertex stage.
    CompositeVs,
    /// `composite.hlsl` pixel stage.
    CompositePs,
    /// `blur.hlsl` fullscreen-triangle vertex stage, shared by blur and resolve.
    FullscreenVs,
    /// `blur.hlsl` separable Gaussian pixel stage.
    BlurPs,
    /// `blur.hlsl` multisample resolve pixel stage.
    ResolvePs,
}

impl Entry {
    /// Every entry point, in the order of the shared shader table.
    pub const ALL: [Entry; 13] = [
        Entry::QuadVs,
        Entry::QuadPs,
        Entry::TextVs,
        Entry::TextPs,
        Entry::TextSubpixelPs,
        Entry::MeshVs,
        Entry::MeshPs,
        Entry::MeshTexturedPs,
        Entry::CompositeVs,
        Entry::CompositePs,
        Entry::FullscreenVs,
        Entry::BlurPs,
        Entry::ResolvePs,
    ];

    #[inline]
    fn index(self) -> usize {
        self as usize
    }

    /// `(file, function, profile)`.
    pub fn location(self) -> (&'static str, &'static str, &'static str) {
        let (_, file, function, profile) = SHADER_TABLE[self.index()];
        (file, function, profile)
    }
}

/// Looks up an embedded shader source by file name.
pub fn source(file: &str) -> Option<&'static str> {
    SOURCES.iter().find(|(f, _)| *f == file).map(|(_, s)| *s)
}

/// The full text handed to the compiler for `file`.
pub fn compose(file: &str) -> Option<String> {
    Some(compose_shader_source(COMMON, file, source(file)?))
}

/// Compiles one entry point to DXBC at runtime.
pub fn compile(entry: Entry) -> Result<Vec<u8>, ShaderError> {
    let (file, function, profile) = entry.location();
    let name = format!("{file}:{function}");
    let source = compose(file)
        .ok_or_else(|| ShaderError { name: name.clone(), message: "source not embedded".into() })?;
    let c = |s: &str| {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    };
    let (file_c, function_c, profile_c) = (c(file), c(function), c(profile));

    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: every pointer handed over is valid for the duration of the call:
    // the source and the three NUL-terminated strings are owned locals, and the
    // two out-parameters are `Option<ID3DBlob>` slots the binding fills.
    let result = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            PCSTR(file_c.as_ptr()),
            None,
            None,
            PCSTR(function_c.as_ptr()),
            PCSTR(profile_c.as_ptr()),
            SHADER_COMPILE_FLAGS,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    let diagnostics = errors.as_ref().map(blob_to_string).unwrap_or_default();
    match (result, code) {
        (Ok(()), Some(code)) => Ok(blob_bytes(&code).to_vec()),
        (Ok(()), None) => {
            Err(ShaderError { name, message: "compiler returned no bytecode".into() })
        }
        (Err(e), _) => Err(ShaderError {
            name,
            message: if diagnostics.trim().is_empty() { e.to_string() } else { diagnostics },
        }),
    }
}

/// Every entry point's bytecode.
pub struct CompiledShaders {
    code: Vec<Cow<'static, [u8]>>,
    /// Wall time spent obtaining the bytecode: the compiler's time for
    /// [`compile_all`], effectively zero when [`load`] used the embedded set.
    pub elapsed: Duration,
    /// True when the bytecode came from the build script rather than a
    /// runtime compile.
    pub precompiled: bool,
}

impl CompiledShaders {
    /// The bytecode for `entry`.
    #[inline]
    pub fn get(&self, entry: Entry) -> &[u8] {
        &self.code[entry.index()]
    }
}

/// Compiles every entry point from source, timing the whole batch.
pub fn compile_all() -> Result<CompiledShaders, ShaderError> {
    let start = Instant::now();
    let mut code = Vec::with_capacity(Entry::ALL.len());
    for entry in Entry::ALL {
        code.push(Cow::Owned(compile(entry)?));
    }
    let elapsed = start.elapsed();
    tracing::debug!(
        target: "spherekit_d3d11",
        ms = elapsed.as_secs_f64() * 1000.0,
        "compiled {} HLSL entry points at runtime",
        code.len()
    );
    Ok(CompiledShaders { code, elapsed, precompiled: false })
}

/// The bytecode the renderer uses: embedded when the build precompiled it,
/// otherwise compiled now.
pub fn load() -> Result<CompiledShaders, ShaderError> {
    #[cfg(spherekit_precompiled_shaders)]
    {
        Ok(CompiledShaders {
            code: PRECOMPILED.iter().map(|c| Cow::Borrowed(*c)).collect(),
            elapsed: Duration::ZERO,
            precompiled: true,
        })
    }
    #[cfg(not(spherekit_precompiled_shaders))]
    {
        compile_all()
    }
}

fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    // SAFETY: a blob owns `GetBufferSize` bytes at `GetBufferPointer` for as
    // long as it lives, and the returned slice borrows the blob.
    unsafe {
        core::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize())
    }
}

fn blob_to_string(blob: &ID3DBlob) -> String {
    String::from_utf8_lossy(blob_bytes(blob)).trim_end_matches('\0').to_string()
}
