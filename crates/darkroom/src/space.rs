//! Typed pixel buffers and color-space markers.
//!
//! `Buffer<S>` is a planar f32 RGB buffer phantom-typed by its color space. The
//! data layout is one `Vec<f32>` of length `3 * width * height`, with channels
//! stored sequentially (`R..R G..G B..B`). Planar layout lets a SIMD load pull
//! 8 same-channel pixels in one instruction with no shuffle.
//!
//! Same-layout reinterpretation (e.g. OKLab ↔ OKLCh, or matrix multiplies that
//! happen to land in a different primary system) reuses the underlying `Vec`
//! via `Buffer::into_space`. Allocation only happens when shape genuinely
//! changes (decode, resize).

use std::marker::PhantomData;

/// Marker trait for color-space tags. The tag is a phantom type parameter on
/// `Buffer<S>` and carries no data.
pub trait ColorSpace {}

/// Camera-native linear RGB. Output of libraw with `output_color = Raw` and
/// `use_camera_wb = false`. Has not had WB or the cam→working matrix applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraLinear;
impl ColorSpace for CameraLinear {}

/// Linear RGB in Rec.2020 (BT.2020) primaries. The MVP working space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinearRec2020;
impl ColorSpace for LinearRec2020 {}

/// Linear RGB in BT.709 (sRGB) primaries. Intermediate before encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinearSrgb;
impl ColorSpace for LinearSrgb {}

/// OKLab — perceptually uniform Cartesian color space. Channels: L, a, b.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Oklab;
impl ColorSpace for Oklab {}

/// OKLab in polar form. Channels: L, C, h (hue in radians).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Oklch;
impl ColorSpace for Oklch {}

/// Planar f32 buffer typed by its color space. Holds one `Vec<f32>` of length
/// `3 * width * height` laid out as `[R..R, G..G, B..B]`.
#[derive(Debug, Clone)]
pub struct Buffer<S: ColorSpace> {
    data: Vec<f32>,
    width: u32,
    height: u32,
    _space: PhantomData<S>,
}

impl<S: ColorSpace> Buffer<S> {
    /// Allocate a zeroed buffer of the given dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        let plane = (width as usize) * (height as usize);
        Self {
            data: vec![0.0; plane * 3],
            width,
            height,
            _space: PhantomData,
        }
    }

    /// Build from an existing planar `Vec<f32>`. Length must equal
    /// `3 * width * height`.
    pub fn from_planar(data: Vec<f32>, width: u32, height: u32) -> Self {
        debug_assert_eq!(
            data.len(),
            (width as usize) * (height as usize) * 3,
            "planar f32 length must be 3 * w * h"
        );
        Self {
            data,
            width,
            height,
            _space: PhantomData,
        }
    }

    /// Build from interleaved `Vec<f32>` (`[R, G, B, R, G, B, ...]`). Used at
    /// I/O boundaries (e.g. when a decoder hands back interleaved pixels).
    pub fn from_interleaved(rgb: &[f32], width: u32, height: u32) -> Self {
        let plane = (width as usize) * (height as usize);
        debug_assert_eq!(
            rgb.len(),
            plane * 3,
            "interleaved f32 length must be 3 * w * h"
        );
        let mut data = vec![0.0f32; plane * 3];
        let (r, gb) = data.split_at_mut(plane);
        let (g, b) = gb.split_at_mut(plane);
        for (i, px) in rgb.chunks_exact(3).enumerate() {
            r[i] = px[0];
            g[i] = px[1];
            b[i] = px[2];
        }
        Self {
            data,
            width,
            height,
            _space: PhantomData,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    pub fn plane_size(&self) -> usize {
        (self.width as usize) * (self.height as usize)
    }

    /// Reinterpret as a different color space without touching the data. The
    /// caller is responsible for having performed any required math (e.g. via
    /// `InPlaceTransform`) before rebinding the type.
    pub fn into_space<T: ColorSpace>(self) -> Buffer<T> {
        Buffer {
            data: self.data,
            width: self.width,
            height: self.height,
            _space: PhantomData,
        }
    }

    /// Consume the buffer and return its raw planar `Vec<f32>`.
    pub fn into_data(self) -> Vec<f32> {
        self.data
    }

    pub fn data(&self) -> &[f32] {
        &self.data
    }
    pub fn data_mut(&mut self) -> &mut [f32] {
        &mut self.data
    }

    pub fn r(&self) -> &[f32] {
        let p = self.plane_size();
        &self.data[..p]
    }
    pub fn g(&self) -> &[f32] {
        let p = self.plane_size();
        &self.data[p..2 * p]
    }
    pub fn b(&self) -> &[f32] {
        let p = self.plane_size();
        &self.data[2 * p..]
    }

    /// Disjoint mutable references to the three channel planes.
    pub fn rgb_planes_mut(&mut self) -> (&mut [f32], &mut [f32], &mut [f32]) {
        let p = self.plane_size();
        let (r, rest) = self.data.split_at_mut(p);
        let (g, b) = rest.split_at_mut(p);
        (r, g, b)
    }
}

/// Display-referred 8-bit sRGB buffer. The output of the develop pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Srgb8 {
    pub width: u32,
    pub height: u32,
    /// Row-major sRGB 8-bit RGB. `pixels.len() == width * height * 3`.
    pub pixels: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_allocates_zero_buffer_of_correct_size() {
        let b: Buffer<LinearRec2020> = Buffer::new(8, 4);
        assert_eq!(b.dimensions(), (8, 4));
        assert_eq!(b.plane_size(), 32);
        assert_eq!(b.data().len(), 96);
        assert!(b.data().iter().all(|&v| v == 0.0));
    }

    #[test]
    fn from_planar_round_trips() {
        let data: Vec<f32> = (0..24).map(|i| i as f32).collect();
        let b: Buffer<LinearRec2020> = Buffer::from_planar(data.clone(), 4, 2);
        assert_eq!(b.r(), &data[0..8]);
        assert_eq!(b.g(), &data[8..16]);
        assert_eq!(b.b(), &data[16..24]);
    }

    #[test]
    fn from_interleaved_deinterleaves_correctly() {
        // 2x1 image: pixels (1, 2, 3) and (4, 5, 6).
        let interleaved = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let b: Buffer<LinearRec2020> = Buffer::from_interleaved(&interleaved, 2, 1);
        assert_eq!(b.r(), &[1.0, 4.0]);
        assert_eq!(b.g(), &[2.0, 5.0]);
        assert_eq!(b.b(), &[3.0, 6.0]);
    }

    #[test]
    fn into_space_preserves_data_and_dims() {
        let data: Vec<f32> = (0..12).map(|i| i as f32).collect();
        let a: Buffer<LinearRec2020> = Buffer::from_planar(data.clone(), 2, 2);
        let dims = a.dimensions();
        let b: Buffer<LinearSrgb> = a.into_space();
        assert_eq!(b.dimensions(), dims);
        assert_eq!(b.into_data(), data);
    }

    #[test]
    fn rgb_planes_mut_yields_disjoint_mut_slices() {
        let mut b: Buffer<LinearRec2020> = Buffer::new(2, 2);
        let (r, g, bl) = b.rgb_planes_mut();
        r[0] = 1.0;
        g[1] = 2.0;
        bl[2] = 3.0;
        assert_eq!(b.r()[0], 1.0);
        assert_eq!(b.g()[1], 2.0);
        assert_eq!(b.b()[2], 3.0);
    }
}
