//! Xbox Series shaders to host shaders.
//!
//! An Xbox Series shader is a standard DXBC container ([`dxbc`]). Its `DXIL`
//! part ([`dxil`]) may carry Xbox additions around the LLVM bitcode: an
//! `XDAT` table before it indexing blobs after it, among them `xbop`
//! (compiler options) and the zlib-compressed GFX10 code. The bitcode may use
//! console-only intrinsics, so the GFX10 code is translated in general.

pub mod bvh;
pub mod dxbc;
pub mod dxil;
pub mod emit;
pub mod gfx10;
pub mod io;
pub mod ir;
pub mod native;
pub mod resource;
pub mod spill;
pub mod structure;

/// The version of what the translator makes, part of the key of every
/// cached translation (`victory-gpu`'s `shader_cache`): **bump it with any
/// change that changes the translation of a shader** (its DXIL or what the
/// host binds for it). Changes that do not (speed, diagnostics, tests,
/// shaders that did not translate before) leave it, and every shader
/// translated before is kept across the rebuild. `victory shaders`
/// translates again what an earlier build of the translator cached and says
/// how many came out different: if any did, the bump was due.
///
/// History: 1 = this key's first (the build of 520776a0, tiled cascade shadows exact on 32-lane
/// hosts); 2 = the stricter proof of which vector values are the same in every lane of a console
/// wave (`lower::lane_uniform`, from the Belobug fork): five programs of the title's pack
/// translate differently (`deferred_tiled:CS_DepthProcessingHBO`,
/// `indirect_light_probes:CS_Primary_RegisterAllocationRequests`,
/// `tiled_lighting:CS_ClusterLightsWorldSpace` and `...IntensityBased`,
/// `water_interaction_foam:CS_DisturbersContribution`) on a host of 32-lane waves;
/// 3 = console threads of 2-D and 3-D groups of more than one host wave come from the wave
/// counter (`VICTORY_THREAD_TICKETS`, Belobug PR #442).
pub const TRANSLATOR_VERSION: u32 = 3;

pub use dxbc::{Container, FourCC, Part};
pub use dxil::{DxilProgram, ShaderKind, XboxExtras};
pub use native::{
    HwStage, NativeProgram, ResourceType, RootParam, RootType, UsedResource, native_programs,
};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ShaderError {
    #[error("not a DXBC container")]
    NotDxbc,
    #[error("malformed shader: {0}")]
    Malformed(&'static str),
    #[error("GFX10 code at {offset:#x}: {reason}")]
    Isa { offset: usize, reason: &'static str },
    /// Valid code the translator cannot handle yet: an opcode name or a construct.
    #[error("GFX10 code at {offset:#x}: {what} is not supported")]
    Unsupported { offset: usize, what: &'static str },
}
