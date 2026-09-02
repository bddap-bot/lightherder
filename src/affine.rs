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

/// Framing of a camera pointed at a monitor: how the monitor's image is
/// magnified, turned and shifted on its way back onto that monitor.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Framing {
    /// >1 magnifies the image once per pass.
    pub zoom: f32,
    /// Counter-clockwise on screen, radians per pass.
    pub rotation: f32,
    /// Shift per pass, in screen units where the monitor is 1.0 tall.
    pub translate: [f32; 2],
    /// Mirror the framed picture left for right.
    pub flip_x: bool,
    /// Mirror the framed picture top for bottom.
    pub flip_y: bool,
}

impl Framing {
    /// A camera that reproduces its subject exactly: no zoom, no turn, no
    /// shift, no mirror. The serde default, so a config can frame only the
    /// cameras that move.
    pub fn identity() -> Framing {
        Framing {
            zoom: 1.0,
            rotation: 0.0,
            translate: [0.0, 0.0],
            flip_x: false,
            flip_y: false,
        }
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
/// The flips are the last stage of the framing, so they mirror the picture the
/// camera has already panned, turned and zoomed: a pan right on a mirrored
/// path lands left. A mirror is its own inverse, so the forward flip is the
/// one composed here.
pub fn sample_transform(framing: &Framing, aspect: f32) -> Affine2 {
    let inv_zoom = 1.0 / framing.zoom;
    let mirror = |on: bool| if on { -1.0 } else { 1.0 };
    uv_to_screen(aspect)
        .then(&Affine2::scale(
            mirror(framing.flip_x),
            mirror(framing.flip_y),
        ))
        .then(&Affine2::translation(
            -framing.translate[0],
            -framing.translate[1],
        ))
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

    fn framing(zoom: f32, rotation: f32, translate: [f32; 2]) -> Framing {
        Framing {
            zoom,
            rotation,
            translate,
            ..Framing::identity()
        }
    }

    #[test]
    fn a_flip_mirrors_the_framed_picture_about_the_centre() {
        let flipped = |flip_x, flip_y| Framing {
            flip_x,
            flip_y,
            ..Framing::identity()
        };
        // The right edge shows what was at the left, at the same height.
        let t = sample_transform(&flipped(true, false), 16.0 / 9.0);
        assert!(
            close(t.apply([1.0, 0.3]), [0.0, 0.3]),
            "{:?}",
            t.apply([1.0, 0.3])
        );
        assert!(close(t.apply([0.5, 0.5]), [0.5, 0.5]));
        // The bottom shows what was at the top.
        let t = sample_transform(&flipped(false, true), 16.0 / 9.0);
        assert!(
            close(t.apply([0.3, 1.0]), [0.3, 0.0]),
            "{:?}",
            t.apply([0.3, 1.0])
        );
        // Both is a half turn.
        let t = sample_transform(&flipped(true, true), 1.0);
        assert!(close(t.apply([1.0, 1.0]), [0.0, 0.0]));
    }

    #[test]
    fn the_flip_mirrors_the_pan_and_not_the_other_way_round() {
        // The left edge shows the spot that was a quarter right of centre:
        // panned to the half, then mirrored. Had the mirror come first the
        // left edge would read from 1.25, off the monitor.
        let t = sample_transform(
            &Framing {
                translate: [0.25, 0.0],
                flip_x: true,
                ..Framing::identity()
            },
            1.0,
        );
        assert!(
            close(t.apply([0.0, 0.5]), [0.75, 0.5]),
            "{:?}",
            t.apply([0.0, 0.5])
        );
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
        let t = sample_transform(&framing(1.0, 0.0, [0.0, 0.0]), 16.0 / 9.0);
        for p in [[0.3, 0.7], [0.0, 0.0], [1.0, 1.0]] {
            assert!(close(t.apply(p), p), "{:?} -> {:?}", p, t.apply(p));
        }
    }

    #[test]
    fn zoom_pulls_the_sample_toward_the_centre() {
        let t = sample_transform(&framing(2.0, 0.0, [0.0, 0.0]), 1.0);
        assert!(close(t.apply([1.0, 0.5]), [0.75, 0.5]));
        assert!(close(t.apply([0.5, 0.5]), [0.5, 0.5]));
    }

    #[test]
    fn positive_rotation_turns_the_image_counter_clockwise() {
        let t = sample_transform(&framing(1.0, FRAC_PI_2, [0.0, 0.0]), 1.0);
        // The right edge shows what was at the bottom (v = 1), which is the
        // bottom sweeping round to the right: counter-clockwise on screen.
        assert!(
            close(t.apply([1.0, 0.5]), [0.5, 1.0]),
            "{:?}",
            t.apply([1.0, 0.5])
        );
    }

    #[test]
    fn translation_moves_the_image_with_its_sign() {
        // Centre now shows what used to be a quarter-width to its left, i.e.
        // the image slid right.
        let t = sample_transform(&framing(1.0, 0.0, [0.25, 0.0]), 1.0);
        assert!(close(t.apply([0.5, 0.5]), [0.25, 0.5]));

        // And up, which is v downward: screen units are y-up and texture v is
        // not, so this pins a flip a uv-space pan would get backwards.
        let t = sample_transform(&framing(1.0, 0.0, [0.0, 0.25]), 1.0);
        assert!(
            close(t.apply([0.5, 0.5]), [0.5, 0.75]),
            "{:?}",
            t.apply([0.5, 0.5])
        );
    }

    #[test]
    fn pan_is_not_scaled_by_the_zoom_it_composes_with() {
        // Pan is what the camera does before it magnifies, so a quarter-width
        // pan is a quarter width on screen at any zoom. Composing the two the
        // other way round would put the centre at 0.625 instead.
        let t = sample_transform(&framing(2.0, 0.0, [0.25, 0.0]), 1.0);
        assert!(
            close(t.apply([0.75, 0.5]), [0.5, 0.5]),
            "{:?}",
            t.apply([0.75, 0.5])
        );
    }

    #[test]
    fn rotation_accounts_for_a_non_square_monitor() {
        let t = sample_transform(&framing(1.0, FRAC_PI_2, [0.0, 0.0]), 2.0);
        // A quarter turn of a 2:1 monitor overflows its own height; a naive
        // uv-space rotation would land at v = 1.0 instead.
        assert!(
            close(t.apply([1.0, 0.5]), [0.5, 1.5]),
            "{:?}",
            t.apply([1.0, 0.5])
        );
    }
}
