/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Compositing of `color-layers()`.
//! <https://drafts.csswg.org/css-color-6/#color-layers>

use super::{AbsoluteColor, ColorSpace};
use crate::values::specified::Blend;

type Rgb = [f32; 3];

fn luminosity(c: Rgb) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: Rgb) -> Rgb {
    let l = luminosity(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    c.map(|v| {
        let mut v = v;
        if n < 0.0 {
            v = l + (v - l) * l / (l - n);
        }
        if x > 1.0 {
            v = l + (v - l) * (1.0 - l) / (x - l);
        }
        v
    })
}

fn set_luminosity(c: Rgb, l: f32) -> Rgb {
    let d = l - luminosity(c);
    clip_color(c.map(|v| v + d))
}

fn saturation(c: Rgb) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_saturation(c: Rgb, s: f32) -> Rgb {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    c.map(|v| {
        if max == min {
            0.0
        } else if v == max {
            s
        } else if v == min {
            0.0
        } else {
            (v - min) * s / (max - min)
        }
    })
}

fn separable(blend: Blend, cb: f32, cs: f32) -> f32 {
    let hard_light = |cb: f32, cs: f32| {
        if cs <= 0.5 {
            cb * 2.0 * cs
        } else {
            let cs = 2.0 * cs - 1.0;
            cb + cs - cb * cs
        }
    };
    match blend {
        Blend::Multiply => cb * cs,
        Blend::Screen => cb + cs - cb * cs,
        Blend::Overlay => hard_light(cs, cb),
        Blend::Darken => cb.min(cs),
        Blend::Lighten => cb.max(cs),
        Blend::ColorDodge => {
            if cb == 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        },
        Blend::ColorBurn => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        },
        Blend::HardLight => hard_light(cb, cs),
        Blend::SoftLight => {
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                let d = if cb <= 0.25 {
                    ((16.0 * cb - 12.0) * cb + 4.0) * cb
                } else {
                    cb.sqrt()
                };
                cb + (2.0 * cs - 1.0) * (d - cb)
            }
        },
        Blend::Difference => (cb - cs).abs(),
        Blend::Exclusion => cb + cs - 2.0 * cb * cs,
        Blend::PlusLighter => (cb + cs).min(1.0),
        Blend::Normal | Blend::Hue | Blend::Saturation | Blend::Color | Blend::Luminosity => cs,
    }
}

/// <https://drafts.fxtf.org/compositing/#blending>
fn blend_colors(blend: Blend, cb: Rgb, cs: Rgb) -> Rgb {
    match blend {
        Blend::Hue => set_luminosity(set_saturation(cs, saturation(cb)), luminosity(cb)),
        Blend::Saturation => set_luminosity(set_saturation(cb, saturation(cs)), luminosity(cb)),
        Blend::Color => set_luminosity(cs, luminosity(cb)),
        Blend::Luminosity => set_luminosity(cb, luminosity(cs)),
        _ => [0, 1, 2].map(|i| separable(blend, cb[i], cs[i])),
    }
}

/// Composites the layers, the first one at the bottom, with the source-over
/// operator and the given blend mode in sRGB.
pub fn composite(blend: Blend, layers: impl IntoIterator<Item = AbsoluteColor>) -> AbsoluteColor {
    let mut legacy = true;
    let mut backdrop: Option<(Rgb, f32)> = None;
    for layer in layers {
        legacy &= layer.is_legacy_syntax();
        let srgb = layer.to_color_space(ColorSpace::Srgb);
        let [r, g, b, alpha] = *srgb.raw_components();
        let source = [r, g, b];
        backdrop = Some(match backdrop {
            None => (source, alpha),
            Some((cb, ab)) => {
                let blended = blend_colors(blend, cb, source);
                let mixed = [0, 1, 2].map(|i| (1.0 - ab) * source[i] + ab * blended[i]);
                let ao = alpha + ab * (1.0 - alpha);
                let co = [0, 1, 2].map(|i| {
                    if ao == 0.0 {
                        0.0
                    } else {
                        (alpha * mixed[i] + ab * cb[i] * (1.0 - alpha)) / ao
                    }
                });
                (co, ao)
            },
        });
    }
    let ([r, g, b], alpha) = backdrop.unwrap_or(([0.0; 3], 0.0));
    let result = AbsoluteColor::new(ColorSpace::Srgb, r, g, b, alpha);
    if legacy {
        result.into_srgb_legacy()
    } else {
        result
    }
}
