//! Camera-linear → working linear Rec.2020.
//!
//! Applies (a) per-channel white-balance multipliers and (b) the 3×3
//! camera-RGB → linear-Rec.2020 matrix (composed offline from the libraw
//! `cam_to_xyz` and the constant XYZ → Rec.2020 matrix at D65).
//!
//! Both operations are in place on the planar `Buffer<S>`; only the phantom
//! type changes from `CameraLinear` to `LinearRec2020`.

use codec::SensorInfo;
use wide::f32x8;

use crate::simd::apply_3x3_planar;
use crate::space::{Buffer, CameraLinear, LinearRec2020};
use crate::transform::InPlaceTransform;

/// XYZ → linear Rec.2020 (BT.2020 primaries) at D65.
pub(crate) const XYZ_TO_REC2020_D65: [[f32; 3]; 3] = [
    [1.71670, -0.35567, -0.25336],
    [-0.66668, 1.61648, 0.01577],
    [0.01764, -0.04277, 0.94210],
];

/// Composes `xyz_to_rec2020 * cam_to_xyz_3x3` and returns a 3×3 matrix taking
/// camera RGB directly to linear Rec.2020.
pub fn cam_to_rec2020_from_sensor(sensor: &SensorInfo) -> [[f32; 3]; 3] {
    let cam_to_xyz_3x3: [[f32; 3]; 3] = [
        sensor.cam_to_xyz[0],
        sensor.cam_to_xyz[1],
        sensor.cam_to_xyz[2],
    ];
    matmul3x3(XYZ_TO_REC2020_D65, cam_to_xyz_3x3)
}

fn matmul3x3(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let mut s = 0.0f32;
            for k in 0..3 {
                s += a[i][k] * b[k][j];
            }
            out[i][j] = s;
        }
    }
    out
}

#[derive(Debug, Clone, Copy)]
pub struct CameraToWorking {
    /// White balance multipliers: R, G, B (G2 not used for 3-channel demosaic
    /// output). Identity = `[1.0, 1.0, 1.0]`.
    pub camera_wb: [f32; 3],
    /// Camera RGB → linear Rec.2020 matrix.
    pub cam_to_rec2020: [[f32; 3]; 3],
}

impl CameraToWorking {
    /// Build from a `SensorInfo` using the camera's as-shot WB and the libraw
    /// `cam_to_xyz` matrix composed with the D65 XYZ → Rec.2020 matrix.
    pub fn from_sensor(sensor: &SensorInfo) -> Self {
        // Normalize WB so the green channel is 1.0. Cameras report cam_mul as
        // raw-domain multipliers; without normalization the entire image
        // brightens/dims with WB.
        let wb = sensor.camera_wb;
        let g = if wb[1] > 0.0 { wb[1] } else { 1.0 };
        let camera_wb = [wb[0] / g, 1.0, wb[2] / g];
        Self {
            camera_wb,
            cam_to_rec2020: cam_to_rec2020_from_sensor(sensor),
        }
    }

    /// Identity transform: WB = (1, 1, 1), matrix = identity.
    pub fn identity() -> Self {
        Self {
            camera_wb: [1.0, 1.0, 1.0],
            cam_to_rec2020: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }
}

impl InPlaceTransform for CameraToWorking {
    type In = CameraLinear;
    type Out = LinearRec2020;
    fn apply(&self, mut src: Buffer<CameraLinear>) -> Buffer<LinearRec2020> {
        let (r, g, b) = src.rgb_planes_mut();
        apply_wb(r, g, b, self.camera_wb);
        apply_3x3_planar(r, g, b, self.cam_to_rec2020);
        src.into_space()
    }
}

fn apply_wb(r: &mut [f32], g: &mut [f32], b: &mut [f32], wb: [f32; 3]) {
    let wr = f32x8::splat(wb[0]);
    let wg = f32x8::splat(wb[1]);
    let wb_v = f32x8::splat(wb[2]);
    let n = r.len();
    let main = n - n % 8;
    let mut i = 0;
    while i < main {
        let vr = f32x8::new(r[i..i + 8].try_into().expect("8 lanes")) * wr;
        let vg = f32x8::new(g[i..i + 8].try_into().expect("8 lanes")) * wg;
        let vb = f32x8::new(b[i..i + 8].try_into().expect("8 lanes")) * wb_v;
        r[i..i + 8].copy_from_slice(&vr.to_array());
        g[i..i + 8].copy_from_slice(&vg.to_array());
        b[i..i + 8].copy_from_slice(&vb.to_array());
        i += 8;
    }
    for k in main..n {
        r[k] *= wb[0];
        g[k] *= wb[1];
        b[k] *= wb[2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_noop() {
        let buf: Buffer<CameraLinear> =
            Buffer::from_planar((0..24).map(|i| i as f32).collect(), 4, 2);
        let original = buf.data().to_vec();
        let xform = CameraToWorking::identity();
        let out = xform.apply(buf);
        assert_eq!(out.data(), original.as_slice());
    }

    #[test]
    fn wb_doubles_red_only() {
        let buf: Buffer<CameraLinear> = Buffer::from_planar(vec![0.5; 24], 4, 2);
        let xform = CameraToWorking {
            camera_wb: [2.0, 1.0, 1.0],
            cam_to_rec2020: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        };
        let out = xform.apply(buf);
        for &v in out.r() {
            assert!((v - 1.0).abs() < 1e-6);
        }
        for &v in out.g() {
            assert!((v - 0.5).abs() < 1e-6);
        }
        for &v in out.b() {
            assert!((v - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn matmul3x3_identity_returns_other() {
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let m = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]];
        assert_eq!(matmul3x3(id, m), m);
        assert_eq!(matmul3x3(m, id), m);
    }
}
