//! 2D affine transforms, and the one that matters: the map from a point on the
//! monitor being rendered back to the point on the previous frame that the
//! camera saw there.

/// A 2D affine map `p -> m * p + t`, with `m` indexed `m[row][col]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine2 {
    pub m: [[f32; 2]; 2],
    pub t: [f32; 2],
}

impl Affine2 {
    /// Counter-clockwise by `radians`, in a y-up space.
    pub fn rotation(radians: f32) -> Affine2 {
        let (s, c) = radians.sin_cos();
        Affine2 {
            m: [[c, -s], [s, c]],
            t: [0.0, 0.0],
        }
    }

    pub fn scale(x: f32, y: f32) -> Affine2 {
        Affine2 {
            m: [[x, 0.0], [0.0, y]],
            t: [0.0, 0.0],
        }
    }

    pub fn translation(x: f32, y: f32) -> Affine2 {
        Affine2 {
            m: [[1.0, 0.0], [0.0, 1.0]],
            t: [x, y],
        }
    }

    pub fn apply(&self, p: [f32; 2]) -> [f32; 2] {
        [
            self.m[0][0] * p[0] + self.m[0][1] * p[1] + self.t[0],
            self.m[1][0] * p[0] + self.m[1][1] * p[1] + self.t[1],
        ]
    }

    /// `self` first, then `next`.
    pub fn then(&self, next: &Affine2) -> Affine2 {
        let m = [
            [
                next.m[0][0] * self.m[0][0] + next.m[0][1] * self.m[1][0],
                next.m[0][0] * self.m[0][1] + next.m[0][1] * self.m[1][1],
            ],
            [
                next.m[1][0] * self.m[0][0] + next.m[1][1] * self.m[1][0],
                next.m[1][0] * self.m[0][1] + next.m[1][1] * self.m[1][1],
            ],
        ];
        Affine2 {
            m,
            t: next.apply(self.t),
        }
    }

    /// Rows `[m00, m01, t0]` and `[m10, m11, t1]`, ready for a shader that
    /// evaluates `dot(row, vec3(uv, 1.0))`.
    pub fn rows(&self) -> [[f32; 3]; 2] {
        [
            [self.m[0][0], self.m[0][1], self.t[0]],
            [self.m[1][0], self.m[1][1], self.t[1]],
        ]
    }
}

/// Where a shaft stands: how the image a camera on it sees is magnified and
/// turned on its way back onto the monitor. The rig's two freedoms and no
/// others — a camera on a shaft slides and turns, and nothing on it pans.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Framing {
    /// >1 magnifies the image once per pass.
    pub zoom: f32,
    /// Counter-clockwise on screen, radians per pass.
    pub rotation: f32,
}

impl Framing {
    /// A camera that reproduces its subject exactly: no zoom, no turn.
    pub fn identity() -> Framing {
        Framing {
            zoom: 1.0,
            rotation: 0.0,
        }
    }
}

/// In the order the pair of flips is written down, which is the order
/// [`crate::params::Monitor::flip`] holds them in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
}

impl Axis {
    pub const ALL: [Axis; 2] = [Axis::X, Axis::Y];
}

/// The mirror a router output puts on what it hands a monitor, as a map from
/// the texel being written to the texel the unmirrored output had there. Its
/// own inverse, which is what lets one composition serve both directions.
pub fn flip_uv(flipped: [bool; 2]) -> Affine2 {
    let mirror = |on: bool| if on { -1.0 } else { 1.0 };
    Affine2 {
        m: [[mirror(flipped[0]), 0.0], [0.0, mirror(flipped[1])]],
        t: [f32::from(flipped[0]), f32::from(flipped[1])],
    }
}

impl Default for Framing {
    fn default() -> Framing {
        Framing::identity()
    }
}

/// Centred, y-up and normalised to the monitor's height — the space the
/// framing numbers are expressed in, so a pan of 0.25 means the same fraction
/// of the monitor's height whatever its shape.
pub fn uv_to_screen(aspect: f32) -> Affine2 {
    Affine2 {
        m: [[aspect, 0.0], [0.0, -1.0]],
        t: [-0.5 * aspect, 0.5],
    }
}

/// The inverse of [`uv_to_screen`].
pub fn screen_to_uv(aspect: f32) -> Affine2 {
    Affine2 {
        m: [[1.0 / aspect, 0.0], [0.0, -1.0]],
        t: [0.5, 0.5],
    }
}

/// UV -> UV map from a destination texel to the source texel the camera saw
/// there, i.e. the inverse of the framing.
///
/// The intermediate space is centred, y-up and normalised to the monitor's
/// height, so rotation stays circular on a non-square monitor and the framing
/// numbers mean the same thing at any resolution.
///
pub fn sample_transform(framing: &Framing, aspect: f32) -> Affine2 {
    let inv_zoom = 1.0 / framing.zoom;
    uv_to_screen(aspect)
        .then(&Affine2::rotation(-framing.rotation))
        .then(&Affine2::scale(inv_zoom, inv_zoom))
        .then(&screen_to_uv(aspect))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5
    }

    fn framing(zoom: f32, rotation: f32) -> Framing {
        Framing { zoom, rotation }
    }

    #[test]
    fn a_flip_mirrors_the_output_about_the_centre() {
        let flipped = |x, y| flip_uv([x, y]);
        // The right edge shows what was at the left, at the same height.
        let t = flipped(true, false);
        assert!(
            close(t.apply([1.0, 0.3]), [0.0, 0.3]),
            "{:?}",
            t.apply([1.0, 0.3])
        );
        assert!(close(t.apply([0.5, 0.5]), [0.5, 0.5]));
        // The bottom shows what was at the top.
        let t = flipped(false, true);
        assert!(
            close(t.apply([0.3, 1.0]), [0.3, 0.0]),
            "{:?}",
            t.apply([0.3, 1.0])
        );
        // Both is a half turn.
        let t = flipped(true, true);
        assert!(close(t.apply([1.0, 1.0]), [0.0, 0.0]));
        // Its own inverse, which is what lets one mirror serve both ways.
        assert!(close(t.apply(t.apply([0.3, 0.8])), [0.3, 0.8]));
        assert!(close(flip_uv([false, false]).apply([0.3, 0.8]), [0.3, 0.8]));
    }

    #[test]
    fn then_is_apply_self_then_next() {
        let a = Affine2::rotation(0.7).then(&Affine2::translation(0.3, -0.2));
        let b = Affine2::scale(2.0, 3.0).then(&Affine2::translation(1.0, 1.0));
        let p = [0.31, -0.77];
        assert!(close(a.then(&b).apply(p), b.apply(a.apply(p))));
    }

    #[test]
    fn the_screen_and_uv_maps_are_inverses() {
        for aspect in [1.0, 16.0 / 9.0, 0.5] {
            for p in [[0.0, 0.0], [1.0, 1.0], [0.3, 0.7]] {
                let round_trip = screen_to_uv(aspect).apply(uv_to_screen(aspect).apply(p));
                assert!(
                    close(round_trip, p),
                    "aspect {aspect}: {p:?} -> {round_trip:?}"
                );
            }
        }
        // The convention itself: the centre of the monitor is the origin, and
        // screen y is up while uv v is down.
        assert!(close(uv_to_screen(1.0).apply([0.5, 0.5]), [0.0, 0.0]));
        assert!(close(uv_to_screen(1.0).apply([0.5, 0.0]), [0.0, 0.5]));
    }
    #[test]
    fn identity_framing_samples_where_it_draws() {
        let t = sample_transform(&framing(1.0, 0.0), 16.0 / 9.0);
        for p in [[0.3, 0.7], [0.0, 0.0], [1.0, 1.0]] {
            assert!(close(t.apply(p), p), "{:?} -> {:?}", p, t.apply(p));
        }
    }

    #[test]
    fn zoom_pulls_the_sample_toward_the_centre() {
        let t = sample_transform(&framing(2.0, 0.0), 1.0);
        assert!(close(t.apply([1.0, 0.5]), [0.75, 0.5]));
        assert!(close(t.apply([0.5, 0.5]), [0.5, 0.5]));
    }

    #[test]
    fn positive_rotation_turns_the_image_counter_clockwise() {
        let t = sample_transform(&framing(1.0, FRAC_PI_2), 1.0);
        // The right edge shows what was at the bottom (v = 1), which is the
        // bottom sweeping round to the right: counter-clockwise on screen.
        assert!(
            close(t.apply([1.0, 0.5]), [0.5, 1.0]),
            "{:?}",
            t.apply([1.0, 0.5])
        );
    }

    #[test]
    fn rotation_accounts_for_a_non_square_monitor() {
        let t = sample_transform(&framing(1.0, FRAC_PI_2), 2.0);
        // A quarter turn of a 2:1 monitor overflows its own height; a naive
        // uv-space rotation would land at v = 1.0 instead.
        assert!(
            close(t.apply([1.0, 0.5]), [0.5, 1.5]),
            "{:?}",
            t.apply([1.0, 0.5])
        );
    }
}
