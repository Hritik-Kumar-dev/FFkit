//! Operations: one module per operation (spec section 4).
//!
//! M1 ships the [`Operation`] trait plus registry metadata (id, name,
//! description, accepted input kinds) that drives the operation picker.
//! Per-operation `fields()` / `build()` implementations land in M3/M5.

pub mod compress;
pub mod concat;
pub mod convert;
pub mod extract_audio;
pub mod fields;
pub mod frames;
pub mod gif;
pub mod image_convert;
pub mod resize;
pub mod subtitles;
pub mod thumbnail;
pub mod trim;

use anyhow::Result;

use crate::ffmpeg::builder::CommandSpec;

pub use fields::{BuildContext, FieldContext};

/// Which input media types an operation accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// A single video file.
    Video,
    /// A single audio file.
    Audio,
    /// Either audio or video.
    AudioOrVideo,
    /// A single image file.
    Image,
    /// Multiple files (batch / concat).
    Multiple,
}

/// Static metadata describing one operation in menus and presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationMeta {
    /// Stable identifier used in config and presets.
    pub id: &'static str,
    /// Menu label, e.g. "Compress video".
    pub name: &'static str,
    /// One-line description shown under the menu selection.
    pub description: &'static str,
    /// Which input media types this operation accepts.
    pub accepts: InputKind,
}

/// Every operation implements this trait so the UI stays generic over them.
pub trait Operation {
    /// Stable identifier used in config and presets.
    fn id(&self) -> &'static str;
    /// Menu label, e.g. "Compress video".
    fn name(&self) -> &'static str;
    /// One-line description shown under the menu selection.
    fn description(&self) -> &'static str;
    /// Which input media types this operation accepts.
    fn accepts(&self) -> InputKind;
    /// The parameter fields to render in the form. Pure: probe and
    /// capabilities in, field defaults out.
    fn fields(&self, ctx: &FieldContext) -> Vec<fields::Field>;
    /// Build the argument vector from current field values. Pure function —
    /// the highest-value test surface in the project (spec §5).
    fn build(&self, ctx: &BuildContext) -> Result<CommandSpec>;
}

/// Macro stamping out an operation's struct plus its static registry
/// metadata. The [`Operation`] impl (fields/build) lives in each submodule.
/// M5 implements all eleven; the macro keeps id/name/description next to
/// the code instead of in a far-away table.
macro_rules! simple_op {
    ($type:ident, $id:expr, $name:expr, $desc:expr, $accepts:expr) => {
        /// Operation marker type; see the [`Operation`] impl below.
        #[derive(Debug, Default, Clone, Copy)]
        pub struct $type;

        impl $type {
            /// Static metadata for the operation picker registry.
            pub const META: crate::ops::OperationMeta = crate::ops::OperationMeta {
                id: $id,
                name: $name,
                description: $desc,
                accepts: $accepts,
            };
        }
    };
}

pub(crate) use simple_op;

/// Registry of all v1 operations, in menu order. The picker renders this.
pub const OPERATIONS: &[OperationMeta] = &[
    convert::ConvertOp::META,
    compress::CompressOp::META,
    trim::TrimOp::META,
    extract_audio::ExtractAudioOp::META,
    resize::ResizeOp::META,
    gif::GifOp::META,
    thumbnail::ThumbnailOp::META,
    frames::FramesOp::META,
    concat::ConcatOp::META,
    subtitles::SubtitlesOp::META,
    image_convert::ImageConvertOp::META,
];

/// Look up an operation by its stable [`OperationMeta::id`].
pub fn find_by_id(id: &str) -> Option<&'static OperationMeta> {
    OPERATIONS.iter().find(|op| op.id == id)
}

/// Instantiate the operation for `id`. Returns `None` for unknown ids
/// (e.g. a preset referencing a removed operation — the caller reports it
/// instead of panicking).
pub fn operation_for(id: &str) -> Option<Box<dyn Operation>> {
    match id {
        "convert" => Some(Box::new(convert::ConvertOp)),
        "compress" => Some(Box::new(compress::CompressOp)),
        "trim" => Some(Box::new(trim::TrimOp)),
        "extract-audio" => Some(Box::new(extract_audio::ExtractAudioOp)),
        "resize" => Some(Box::new(resize::ResizeOp)),
        "gif" => Some(Box::new(gif::GifOp)),
        "thumbnail" => Some(Box::new(thumbnail::ThumbnailOp)),
        "frames" => Some(Box::new(frames::FramesOp)),
        "concat" => Some(Box::new(concat::ConcatOp)),
        "subtitles" => Some(Box::new(subtitles::SubtitlesOp)),
        "image-convert" => Some(Box::new(image_convert::ImageConvertOp)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn registry_has_all_eleven_operations() {
        assert_eq!(OPERATIONS.len(), 11);
    }

    #[test]
    fn operation_ids_are_unique_and_non_empty() {
        let mut seen = HashSet::new();
        for op in OPERATIONS {
            assert!(!op.id.is_empty(), "operation id must not be empty");
            assert!(!op.name.is_empty(), "operation name must not be empty");
            assert!(
                !op.description.is_empty(),
                "operation description must not be empty"
            );
            assert!(seen.insert(op.id), "duplicate operation id: {}", op.id);
        }
    }

    #[test]
    fn find_by_id_round_trips() {
        for op in OPERATIONS {
            assert_eq!(find_by_id(op.id), Some(op));
        }
        assert_eq!(find_by_id("no-such-operation"), None);
    }
}
