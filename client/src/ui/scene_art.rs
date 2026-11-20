//! What a colour theme draws over its gradient (`theme::scenes`): a still picture as SVG (a
//! skyline in neon, a city in winter, mountains under the moon, a page of a notebook, a world of
//! blocks) and what moves in front of it (rain and glitches, snow, embers, a paper plane, clouds).
//! Everything is drawn from a fixed seed, so a theme looks the same every time.

use gpui::{AnyElement, IntoElement, ParentElement, Styled, div, px};
use std::fmt::Write;
use std::sync::Arc;

/// A small, fixed sequence of numbers: the same picture every time.
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Rng {
        Rng(seed.max(1))
    }

    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }

    /// 0..1.
    fn f(&mut self) -> f32 {
        (self.next() % 10_000) as f32 / 10_000.
    }

    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }

    fn pick<T: Copy>(&mut self, of: &[T]) -> T {
        of[(self.next() as usize) % of.len()]
    }
}

/// A number from `i` and `salt` alone, 0..1: what each moving thing is, without keeping any.
fn hash(i: u32, salt: u32) -> f32 {
    let mut r = Rng::new(i.wrapping_mul(2_654_435_761) ^ salt.wrapping_mul(40_503).wrapping_add(17));
    r.next();
    r.f()
}

/// Whether `scene` has a picture over its gradient.
pub fn has_art(scene: &str) -> bool {
    matches!(scene, "nightcity" | "division" | "middleearth" | "bullworth" | "blocks" | "outerwilds")
}

/// The picture for `scene` over an area `w` by `h`, or None for one that has only its gradient.
pub fn art(scene: &str, w: f32, h: f32) -> Option<String> {
    let body = match scene {
        "nightcity" => night_city(w, h),
        "division" => division(w, h),
        "middleearth" => middle_earth(w, h),
        "bullworth" => bullworth(w, h),
        "blocks" => blocks(w, h),
        "outerwilds" => outer_wilds(w, h),
        _ => return None,
    };
    Some(format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}">{body}</svg>"#))
}

/// A row of buildings along the bottom: `(x, width, height)` each, from `x0` to past `w`.
fn skyline(r: &mut Rng, w: f32, widths: (f32, f32), heights: (f32, f32)) -> Vec<(f32, f32, f32)> {
    let mut x = -r.range(0., widths.1);
    let mut out = Vec::new();
    while x < w {
        let bw = r.range(widths.0, widths.1);
        out.push((x, bw, r.range(heights.0, heights.1)));
        x += bw + r.range(0., 6.);
    }
    out
}

/// Lit windows on a building, `p` of them on.
fn windows(s: &mut String, r: &mut Rng, (x, bw, bh): (f32, f32, f32), h: f32, p: f32, colours: &[&str], opacity: (f32, f32)) {
    let (cw, ch) = (9., 12.);
    let cols = ((bw - 8.) / cw).floor() as i32;
    let rows = ((bh - 14.) / ch).floor() as i32;
    for row in 0..rows.max(0) {
        for col in 0..cols.max(0) {
            if r.f() < p {
                let _ = write!(
                    s,
                    r#"<rect x="{:.1}" y="{:.1}" width="3" height="4" fill="{}" opacity="{:.2}"/>"#,
                    x + 5. + col as f32 * cw,
                    h - bh + 10. + row as f32 * ch,
                    r.pick(colours),
                    r.range(opacity.0, opacity.1)
                );
            }
        }
    }
}

/// Night City: a skyline in neon, its windows lit in the game's yellow, cyan and red, signs
/// glowing, scanlines over all of it and the interface's corner brackets.
fn night_city(w: f32, h: f32) -> String {
    let mut r = Rng::new(2077);
    let mut s = String::from(
        r##"<defs><radialGradient id="haze" cx="0.55" cy="1" r="0.75"><stop offset="0" stop-color="#ff003c" stop-opacity="0.32"/><stop offset="0.5" stop-color="#7a0bc0" stop-opacity="0.14"/><stop offset="1" stop-color="#000" stop-opacity="0"/></radialGradient><pattern id="scan" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="1.2" fill="#000" opacity="0.22"/></pattern><linearGradient id="sky" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#5a1aa0" stop-opacity="0.35"/><stop offset="0.5" stop-color="#02d7f2" stop-opacity="0.05"/><stop offset="1" stop-color="#000" stop-opacity="0"/></linearGradient><linearGradient id="holo" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#ff2a6d"/><stop offset="1" stop-color="#02d7f2"/></linearGradient></defs>"##,
    );
    let _ = write!(s, r#"<rect width="{w}" height="{h}" fill="url(#haze)"/><rect width="{w}" height="{h}" fill="url(#sky)"/>"#);
    let colours = ["#fcee0a", "#02d7f2", "#ff2a6d", "#02d7f2"];
    // Far off, the megabuildings in the haze, with a few lights up their sides.
    for b in skyline(&mut r, w, (60., 150.), (h * 0.5, h * 0.85)) {
        let _ = write!(
            s,
            r##"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="#1b0f38" opacity="0.75"/>"##,
            b.0,
            h - b.2,
            b.1,
            b.2
        );
        windows(&mut s, &mut r, b, h, 0.04, &["#7a5cff", "#02d7f2"], (0.2, 0.45));
    }
    // One tower above all of them, lit in lines up its face, wearing a hologram.
    let (tx, tw) = (w * r.range(0.18, 0.32), 150.);
    let top = h * 0.12;
    let _ = write!(
        s,
        r##"<path d="M{tx:.1} {h}V{:.1}L{:.1} {top:.1}H{:.1}L{:.1} {:.1}V{h}Z" fill="#150a2c"/>"##,
        top + 40.,
        tx + 30.,
        tx + tw - 30.,
        tx + tw,
        top + 40.
    );
    for k in 0..5 {
        let lx = tx + 22. + k as f32 * 26.;
        let _ = write!(
            s,
            r##"<rect x="{lx:.1}" y="{:.1}" width="2" height="{:.1}" fill="#02d7f2" opacity="0.35"/>"##,
            top + 50.,
            h - top - 50.
        );
    }
    let (hy, hh) = (top + 90., 120.);
    let _ = write!(
        s,
        r##"<rect x="{:.1}" y="{hy:.1}" width="{:.1}" height="{hh}" fill="url(#holo)" opacity="0.55"/><rect x="{:.1}" y="{hy:.1}" width="{:.1}" height="{hh}" fill="url(#scan)"/>"##,
        tx + 18.,
        tw - 36.,
        tx + 18.,
        tw - 36.
    );
    // The far towers, tall and thin, with a mast here and there.
    for b in skyline(&mut r, w, (34., 80.), (h * 0.3, h * 0.66)) {
        let _ = write!(s, r##"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="#120a26"/>"##, b.0, h - b.2, b.1, b.2);
        if r.f() < 0.35 {
            let mx = b.0 + b.1 / 2.;
            let _ = write!(
                s,
                r##"<rect x="{:.1}" y="{:.1}" width="2" height="{:.1}" fill="#120a26"/><circle cx="{:.1}" cy="{:.1}" r="2.2" fill="#ff003c" opacity="0.9"/>"##,
                mx - 1.,
                h - b.2 - 40.,
                40.,
                mx,
                h - b.2 - 40.
            );
        }
        windows(&mut s, &mut r, b, h, 0.12, &colours, (0.25, 0.6));
    }
    // The near blocks, lower and darker, some wearing a sign.
    for b in skyline(&mut r, w, (70., 170.), (h * 0.12, h * 0.36)) {
        let _ = write!(s, r##"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="#06030d"/>"##, b.0, h - b.2, b.1, b.2);
        windows(&mut s, &mut r, b, h, 0.05, &colours, (0.3, 0.7));
        if r.f() < 0.45 {
            // A vertical neon strip, its glow first.
            let c = r.pick(&["#02d7f2", "#ff2a6d", "#fcee0a"]);
            let (sx, sy, sh) = (b.0 + r.range(8., b.1 - 12.), h - b.2 + r.range(8., 30.), r.range(40., (b.2 - 30.).max(41.)));
            for (wd, o) in [(14., 0.08), (8., 0.16), (3., 0.95)] {
                let _ = write!(
                    s,
                    r#"<rect x="{:.1}" y="{sy:.1}" width="{wd}" height="{sh:.1}" rx="{:.1}" fill="{c}" opacity="{o}"/>"#,
                    sx - wd / 2.,
                    wd / 2.
                );
            }
        }
    }
    // A sign across a roof: an outline in red with its glow.
    let (bx, by, bw, bh) = (w * r.range(0.55, 0.7), h * 0.52, 120., 46.);
    for (sw, o) in [(12., 0.08), (6., 0.18), (2.5, 0.95)] {
        let _ = write!(
            s,
            r##"<rect x="{bx:.1}" y="{by:.1}" width="{bw}" height="{bh}" rx="4" fill="none" stroke="#ff003c" stroke-width="{sw}" opacity="{o}"/>"##
        );
    }
    let _ =
        write!(s, r##"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="6" fill="#fcee0a" opacity="0.85"/>"##, bx + 14., by + 20., bw - 28.);
    // Hazard stripes along the bottom right, as on the game's yellow panels.
    let mut stripes = String::new();
    let mut x = w - 260.;
    while x < w + 30. {
        let _ = write!(stripes, "M{x:.1} {h:.1}L{:.1} {:.1}L{:.1} {:.1}L{:.1} {h:.1}Z", x + 22., h - 22., x + 36., h - 22., x + 14.);
        x += 30.;
    }
    let _ = write!(s, r##"<path d="{stripes}" fill="#fcee0a" opacity="0.45"/>"##);
    // The interface's brackets in the corners.
    let b = 36.;
    let _ = write!(
        s,
        r##"<path d="M14 {b}V14H{b} M{x1} 14H{x2}V{b} M14 {y1}V{y2}H{b}" fill="none" stroke="#fcee0a" stroke-width="3" opacity="0.7"/>"##,
        x1 = w - b,
        x2 = w - 14.,
        y1 = h - b,
        y2 = h - 14.
    );
    let _ = write!(s, r#"<rect width="{w}" height="{h}" fill="url(#scan)"/>"#);
    s
}

/// Winter in Manhattan: stepped towers, water tanks on the roofs, a few warm windows, the orange
/// of an agent's watch low on the horizon and its hexagons in a corner.
fn division(w: f32, h: f32) -> String {
    let mut r = Rng::new(7);
    let mut s = String::from(
        r##"<defs><radialGradient id="glow" cx="0.85" cy="1" r="0.7"><stop offset="0" stop-color="#ff6d10" stop-opacity="0.25"/><stop offset="1" stop-color="#ff6d10" stop-opacity="0"/></radialGradient><linearGradient id="fog" x1="0" y1="0" x2="0" y2="1"><stop offset="0.55" stop-color="#c8d3e0" stop-opacity="0"/><stop offset="1" stop-color="#c8d3e0" stop-opacity="0.12"/></linearGradient></defs>"##,
    );
    let _ = write!(s, r#"<rect width="{w}" height="{h}" fill="url(#glow)"/>"#);
    for (layer, fill, hs, p) in [(0, "#1a2029", (0.35, 0.62), 0.05), (1, "#0c0f14", (0.15, 0.4), 0.07)] {
        for b in skyline(&mut r, w, (50., 130.), (h * hs.0, h * hs.1)) {
            let (x, bw, bh) = b;
            let top = h - bh;
            // Setbacks: two narrower steps on the taller ones.
            let _ = write!(s, r#"<rect x="{x:.1}" y="{top:.1}" width="{bw:.1}" height="{bh:.1}" fill="{fill}"/>"#);
            if layer == 0 && r.f() < 0.5 {
                let (sw, sh) = (bw * 0.6, r.range(16., 40.));
                let _ = write!(
                    s,
                    r#"<rect x="{:.1}" y="{:.1}" width="{sw:.1}" height="{sh:.1}" fill="{fill}"/>"#,
                    x + (bw - sw) / 2.,
                    top - sh
                );
                if r.f() < 0.4 {
                    let _ =
                        write!(s, r#"<rect x="{:.1}" y="{:.1}" width="3" height="46" fill="{fill}"/>"#, x + bw / 2. - 1.5, top - sh - 46.);
                }
            } else if r.f() < 0.4 {
                // A water tank on legs.
                let tx = x + r.range(6., (bw - 22.).max(7.));
                let _ = write!(
                    s,
                    r#"<rect x="{tx:.1}" y="{:.1}" width="16" height="14" rx="2" fill="{fill}"/><path d="M{tx:.1} {:.1}l8 -6l8 6z" fill="{fill}"/><rect x="{:.1}" y="{:.1}" width="2" height="8" fill="{fill}"/><rect x="{:.1}" y="{:.1}" width="2" height="8" fill="{fill}"/>"#,
                    top - 22.,
                    top - 22.,
                    tx + 1.,
                    top - 8.,
                    tx + 13.,
                    top - 8.
                );
            }
            windows(&mut s, &mut r, b, h, p, &["#ffb36b", "#ffd59a", "#9fc6ff"], (0.25, 0.55));
        }
    }
    let _ = write!(s, r#"<rect width="{w}" height="{h}" fill="url(#fog)"/>"#);
    // Hexagons, as the watch's interface draws them.
    let hex = |cx: f32, cy: f32, r: f32| {
        (0..6)
            .map(|k| {
                let a = std::f32::consts::FRAC_PI_3 * k as f32;
                format!("{}{:.1} {:.1}", if k == 0 { "M" } else { "L" }, cx + r * a.cos(), cy + r * a.sin())
            })
            .collect::<String>()
            + "Z"
    };
    let mut d = String::new();
    for row in 0..7 {
        for col in 0..9 {
            if r.f() < 0.55 {
                let cx = 30. + col as f32 * 39. + if row % 2 == 1 { 19.5 } else { 0. };
                d += &hex(cx, 30. + row as f32 * 22.5, 12.);
            }
        }
    }
    let _ = write!(s, r##"<path d="{d}" fill="none" stroke="#ff6d10" stroke-width="1.5" opacity="0.22"/>"##);
    s
}

/// A ridge across the width: heights from midpoint displacement, rough at every scale, as
/// mountains are. `base` and `peak` are fractions of `h` from the top.
fn ridge(r: &mut Rng, w: f32, h: f32, base: f32, peak: f32) -> Vec<(f32, f32)> {
    const STEPS: usize = 128;
    let mut y = vec![0f32; STEPS + 1];
    y[0] = r.range(0.2, 0.6);
    y[STEPS] = r.range(0.2, 0.6);
    let (mut step, mut rough) = (STEPS, 0.9);
    while step > 1 {
        let half = step / 2;
        let mut i = half;
        while i < STEPS {
            y[i] = (y[i - half] + y[i + half]) / 2. + r.range(-rough, rough);
            i += step;
        }
        step = half;
        rough *= 0.55;
    }
    let (lo, hi) = y.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    (0..=STEPS)
        .map(|i| {
            let t = (y[i] - lo) / (hi - lo).max(1e-3);
            (-20. + (w + 40.) * i as f32 / STEPS as f32, h * (base + (peak - base) * t))
        })
        .collect()
}

/// Night over the mountains: stars, a crescent moon, three ranges in the mist, snow on the high
/// peaks of the farthest.
fn middle_earth(w: f32, h: f32) -> String {
    let mut r = Rng::new(3019);
    let (mx, my) = (w * 0.8, h * 0.17);
    let mut s = format!(
        r##"<defs><radialGradient id="halo" cx="0.5" cy="0.5" r="0.5"><stop offset="0.3" stop-color="#f5e9c8" stop-opacity="0.22"/><stop offset="1" stop-color="#f5e9c8" stop-opacity="0"/></radialGradient><mask id="crescent"><circle cx="{mx:.1}" cy="{my:.1}" r="28" fill="#fff"/><circle cx="{:.1}" cy="{:.1}" r="25" fill="#000"/></mask></defs>"##,
        mx + 13.,
        my - 7.
    );
    for _ in 0..((w * h / 9000.) as usize).min(220) {
        let _ = write!(
            s,
            r##"<circle cx="{:.1}" cy="{:.1}" r="{:.2}" fill="#fff8e0" opacity="{:.2}"/>"##,
            r.range(0., w),
            r.range(0., h * 0.6),
            r.range(0.5, 1.5),
            r.range(0.2, 0.8)
        );
    }
    let _ = write!(
        s,
        r##"<circle cx="{mx:.1}" cy="{my:.1}" r="120" fill="url(#halo)"/><circle cx="{mx:.1}" cy="{my:.1}" r="28" fill="#f5e9c8" opacity="0.92" mask="url(#crescent)"/>"##
    );
    for (k, (base, peak, fill)) in [(0.66, 0.26, "#1d3d33"), (0.78, 0.48, "#11291f"), (0.9, 0.66, "#08150f")].into_iter().enumerate() {
        let line = ridge(&mut r, w, h, base, peak);
        let mut d = format!("M-20 {h}");
        for (x, y) in &line {
            let _ = write!(d, "L{x:.1} {y:.1}");
        }
        let _ = write!(d, "L{} {h}Z", w + 20.);
        let _ = write!(s, r#"<path d="{d}" fill="{fill}"/>"#);
        if k == 0 {
            // Snow on whatever rises above the snowline, along the ridge's own shape.
            let line_y = h * (peak + (base - peak) * 0.3);
            let _ = write!(
                s,
                r##"<clipPath id="range{k}"><path d="{d}"/></clipPath><rect x="0" y="0" width="{w}" height="{line_y:.1}" fill="#e8efe6" opacity="0.5" clip-path="url(#range{k})"/>"##
            );
        }
        // Mist in the valley in front of each range.
        let _ = write!(
            s,
            r##"<ellipse cx="{:.1}" cy="{:.1}" rx="{:.1}" ry="28" fill="#cfe3d6" opacity="0.06"/>"##,
            w * r.range(0.2, 0.8),
            h * base,
            w * 0.6
        );
    }
    s
}

/// A page of a school notebook: ruled lines, the red margin and chalk-white doodles.
fn bullworth(w: f32, h: f32) -> String {
    let mut r = Rng::new(2006);
    let mut s = String::new();
    let mut y = 40.;
    while y < h {
        let _ = write!(s, r##"<rect x="0" y="{y:.1}" width="{w}" height="1.5" fill="#9fc3ff" opacity="0.14"/>"##);
        y += 30.;
    }
    let _ = write!(s, r##"<rect x="76" y="0" width="2" height="{h}" fill="#ff6b6b" opacity="0.32"/>"##);
    let doodles = [
        // A star, a heart, an arrow, a spiral, a smiley, a bolt, a crown, noughts and crosses.
        "M0 -22L6 -7L22 -7L9 3L14 19L0 9L-14 19L-9 3L-22 -7L-6 -7Z",
        "M0 18C-30 -4 -18 -26 0 -10C18 -26 30 -4 0 18Z",
        "M-24 10C-8 -6 6 -8 22 -4M12 -14L22 -4L10 4",
        "M0 0C4 -4 8 2 4 6C-2 12 -10 4 -8 -2C-4 -14 12 -14 14 0C16 14 -6 22 -16 10",
        "M0 -20A20 20 0 1 0 0.1 -20ZM-7 -5V-3M7 -5V-3M-9 6C-4 12 4 12 9 6",
        "M6 -24L-8 2H4L-4 24L12 -4H0Z",
        "M-20 10L-22 -12L-9 -2L0 -18L9 -2L22 -12L20 10Z",
        "M-8 -24V24M8 -24V24M-24 -8H24M-24 8H24M-19 -19L-13 -13M-13 -19L-19 -13M16 0A3 3 0 1 0 16.1 0",
    ];
    let (cols, rows) = ((w / 230.).ceil() as i32, (h / 200.).ceil() as i32);
    for row in 0..rows {
        for col in 0..cols {
            if r.f() < 0.6 {
                let (x, y) = (col as f32 * 230. + r.range(60., 170.), row as f32 * 200. + r.range(50., 150.));
                let _ = write!(
                    s,
                    r##"<path d="{}" transform="translate({x:.1} {y:.1}) rotate({:.0}) scale({:.2})" fill="none" stroke="#ffffff" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" opacity="{:.2}"/>"##,
                    r.pick(&doodles),
                    r.range(-25., 25.),
                    r.range(1., 1.8),
                    r.range(0.12, 0.22)
                );
            }
        }
    }
    s
}

/// Where things sit in the Outer Wilds picture: the Sun (high in the middle, over the chat's
/// header rather than behind names), the campfire, the launch tower's top.
struct Hearth {
    sun: (f32, f32),
    fire: (f32, f32),
    tower: (f32, f32),
}

/// Timber Hearth's horizon: a small planet, so the ground curves away. Its height at `x`.
fn hearth_ground(w: f32, h: f32, x: f32) -> f32 {
    let (cx, radius) = (w * 0.22, w.max(h) * 1.1);
    let cy = h - h * 0.24 + radius;
    let dx = (x - cx).clamp(-(radius - 1.), radius - 1.);
    cy - (radius * radius - dx * dx).sqrt()
}

fn hearth(w: f32, h: f32) -> Hearth {
    let (fire_x, tower_x) = (w * 0.18, w * 0.38);
    Hearth { sun: (w * 0.58, h * 0.08), fire: (fire_x, hearth_ground(w, h, fire_x)), tower: (tower_x, hearth_ground(w, h, tower_x) - 150.) }
}

/// Outer Wilds: the solar system over Timber Hearth. Stars, the Sun with its planets on their
/// orbits, Nomai writing spiralling in the dark, and on the ground pines, the wooden launch
/// tower and the campfire.
fn outer_wilds(w: f32, h: f32) -> String {
    let mut r = Rng::new(22);
    let at = hearth(w, h);
    let mut s = String::from(
        r##"<defs><radialGradient id="sun" cx="0.5" cy="0.5" r="0.5"><stop offset="0" stop-color="#ffd27a" stop-opacity="0.55"/><stop offset="0.35" stop-color="#ef7b39" stop-opacity="0.2"/><stop offset="1" stop-color="#ef7b39" stop-opacity="0"/></radialGradient><radialGradient id="fire" cx="0.5" cy="0.5" r="0.5"><stop offset="0" stop-color="#ffb347" stop-opacity="0.45"/><stop offset="1" stop-color="#ef7b39" stop-opacity="0"/></radialGradient><radialGradient id="nebula" cx="0.5" cy="0.5" r="0.5"><stop offset="0" stop-color="#6b4fd8" stop-opacity="0.16"/><stop offset="1" stop-color="#6b4fd8" stop-opacity="0"/></radialGradient></defs>"##,
    );
    let _ =
        write!(s, r#"<ellipse cx="{:.1}" cy="{:.1}" rx="{:.1}" ry="{:.1}" fill="url(#nebula)"/>"#, w * 0.35, h * 0.3, w * 0.4, h * 0.25);
    for _ in 0..((w * h / 7000.) as usize).min(260) {
        let _ = write!(
            s,
            r#"<circle cx="{:.1}" cy="{:.1}" r="{:.2}" fill="{}" opacity="{:.2}"/>"#,
            r.range(0., w),
            r.range(0., h * 0.8),
            r.range(0.5, 1.5),
            r.pick(&["#ffffff", "#cfe0ff", "#ffe9c4"]),
            r.range(0.25, 0.85)
        );
    }
    // The planets on their orbits round the Sun, all tilted alike.
    let (sx, sy) = at.sun;
    let tilt = (-14f32).to_radians();
    let planets = [("#7fbf6a", 5.), ("#c56b3a", 4.), ("#b48cd8", 6.5), ("#3fa7a0", 8.), ("#6a4f8f", 6.)];
    for (rx, (colour, size)) in [130., 210., 300., 410., 540.].into_iter().zip(planets) {
        let ry = rx * 0.34;
        let _ = write!(
            s,
            r##"<ellipse cx="{sx:.1}" cy="{sy:.1}" rx="{rx}" ry="{ry:.1}" fill="none" stroke="#9fd9ff" stroke-width="1.2" opacity="0.13" transform="rotate(-14 {sx:.1} {sy:.1})"/>"##
        );
        let a = r.range(0., std::f32::consts::TAU);
        let (ox, oy) = (rx * a.cos(), ry * a.sin());
        let (px, py) = (sx + ox * tilt.cos() - oy * tilt.sin(), sy + ox * tilt.sin() + oy * tilt.cos());
        let _ = write!(s, r#"<circle cx="{px:.1}" cy="{py:.1}" r="{size}" fill="{colour}"/>"#);
    }
    let _ = write!(
        s,
        r##"<circle cx="{sx:.1}" cy="{sy:.1}" r="150" fill="url(#sun)"/><circle cx="{sx:.1}" cy="{sy:.1}" r="34" fill="#f59a3c"/><circle cx="{sx:.1}" cy="{sy:.1}" r="24" fill="#ffd27a"/>"##
    );
    // Nomai writing: a spiral branching into smaller ones.
    let spiral = |cx: f32, cy: f32, size: f32, turn: f32| {
        let mut d = String::new();
        for i in 0..=60 {
            let t = i as f32 / 60.;
            let a = turn + t * std::f32::consts::TAU * 1.6;
            let rr = size * (1. - t * 0.85);
            let _ = write!(d, "{}{:.1} {:.1}", if i == 0 { "M" } else { "L" }, cx + rr * a.cos(), cy + rr * a.sin());
        }
        d
    };
    let (nx, ny) = (w * 0.08, h * 0.14);
    let nomai = spiral(nx, ny, 46., 0.3) + &spiral(nx + 58., ny + 34., 22., 2.1) + &spiral(nx - 30., ny + 52., 16., 4.);
    let _ = write!(s, r##"<path d="{nomai}" fill="none" stroke="#9fb4ff" stroke-width="2.2" stroke-linecap="round" opacity="0.3"/>"##);
    // The ground, then its pines, leaving room for the fire and the tower.
    let mut ground = format!("M-20 {h}");
    let mut x = -20.;
    while x <= w + 20. {
        let _ = write!(ground, "L{x:.1} {:.1}", hearth_ground(w, h, x));
        x += 16.;
    }
    let _ = write!(ground, "L{} {h}Z", w + 20.);
    let _ = write!(s, r##"<path d="{ground}" fill="#0b1712"/>"##);
    let mut pines = String::new();
    let mut x = -10.;
    while x < w * 0.72 {
        let clear = (x - at.fire.0).abs() < 60. || (x - at.tower.0).abs() < 50.;
        if !clear {
            let base = hearth_ground(w, h, x) + 4.;
            let tall = r.range(36., 84.);
            for k in 0..3 {
                let (top, bottom) = (base - tall + k as f32 * tall * 0.22, base - tall * 0.3 + k as f32 * tall * 0.28);
                let half = tall * 0.42 * (0.55 + k as f32 * 0.25);
                let _ = write!(pines, "M{x:.1} {top:.1}L{:.1} {bottom:.1}L{:.1} {bottom:.1}Z", x - half, x + half);
            }
        }
        x += r.range(18., 46.);
    }
    let _ = write!(s, r##"<path d="{pines}" fill="#06100b"/>"##);
    // The wooden launch tower: two legs, crossed braces, a platform and its light.
    let (tx, ty) = at.tower;
    let foot = hearth_ground(w, h, tx);
    let _ = write!(
        s,
        r##"<g fill="#140d08"><rect x="{l:.1}" y="{ty:.1}" width="5" height="{tall:.1}"/><rect x="{rr:.1}" y="{ty:.1}" width="5" height="{tall:.1}"/><rect x="{px:.1}" y="{py:.1}" width="50" height="6"/></g><path d="M{a:.1} {ty:.1}L{b:.1} {foot:.1}M{b:.1} {ty:.1}L{a:.1} {foot:.1}" stroke="#140d08" stroke-width="3"/><circle cx="{tx:.1}" cy="{ly:.1}" r="3" fill="#ff5a3a" opacity="0.9"/>"##,
        l = tx - 22.,
        rr = tx + 17.,
        tall = foot - ty,
        px = tx - 25.,
        py = ty - 6.,
        a = tx - 20.,
        b = tx + 19.,
        ly = ty - 10.
    );
    // The campfire: its glow, two logs and the flame.
    let (fx, fy) = at.fire;
    let _ = write!(
        s,
        r##"<circle cx="{fx:.1}" cy="{fy:.1}" r="130" fill="url(#fire)"/><path d="M{l:.1} {lo:.1}L{r2:.1} {hi:.1}M{l:.1} {hi:.1}L{r2:.1} {lo:.1}" stroke="#2a1a10" stroke-width="5" stroke-linecap="round"/><path d="M{a:.1} {fy:.1}Q{fx:.1} {t1:.1} {b:.1} {fy:.1}Z" fill="#ef7b39"/><path d="M{c:.1} {fy:.1}Q{fx:.1} {t2:.1} {d:.1} {fy:.1}Z" fill="#ffd27a"/>"##,
        l = fx - 16.,
        r2 = fx + 16.,
        lo = fy + 2.,
        hi = fy - 6.,
        a = fx - 10.,
        b = fx + 10.,
        t1 = fy - 34.,
        c = fx - 5.,
        d = fx + 5.,
        t2 = fy - 20.
    );
    s
}

/// The size of a block in the world of blocks.
const BLOCK: f32 = 24.;

/// Sky in steps, a square sun, hills of grass, dirt and stone, and trees.
fn blocks(w: f32, h: f32) -> String {
    let mut r = Rng::new(64);
    let mut s = String::new();
    let rows = (h / BLOCK).ceil() as i32;
    // The sky lightens toward the horizon in bands of a block.
    for row in 0..rows {
        let t = row as f32 / rows as f32;
        let c = crate::theme::mix_rgb(0x6f9fef, 0xb9d6ff, t.min(1.));
        let _ = write!(s, r##"<rect x="0" y="{:.0}" width="{w}" height="{BLOCK}" fill="#{c:06x}"/>"##, row as f32 * BLOCK);
    }
    let (sx, sy) = ((w * 0.78 / BLOCK).floor() * BLOCK, BLOCK * 2.);
    let _ = write!(
        s,
        r##"<rect x="{sx}" y="{sy}" width="{}" height="{}" fill="#fff3a0"/><rect x="{}" y="{}" width="{BLOCK}" height="{BLOCK}" fill="#fffbe0"/>"##,
        BLOCK * 3.,
        BLOCK * 3.,
        sx + BLOCK,
        sy + BLOCK
    );
    // Each column of the ground: its height in blocks, wandering a little.
    let cols = (w / BLOCK).ceil() as i32 + 1;
    let mut height = 6i32;
    let mut ground = Vec::new();
    for _ in 0..cols {
        height = (height + [-1, 0, 0, 0, 1][(r.next() % 5) as usize]).clamp(4, 9);
        ground.push(height);
    }
    // Pixels of a block: 4 by 4, each a shade of its kind.
    let px = BLOCK / 4.;
    let texture = |s: &mut String, r: &mut Rng, x: f32, y: f32, kind: &[u32]| {
        for j in 0..4 {
            for i in 0..4 {
                let _ = write!(
                    s,
                    r##"<rect x="{:.0}" y="{:.0}" width="{px}" height="{px}" fill="#{:06x}"/>"##,
                    x + i as f32 * px,
                    y + j as f32 * px,
                    r.pick(kind)
                );
            }
        }
    };
    const GRASS: [u32; 4] = [0x7cbd5a, 0x6aa84f, 0x5f9b3a, 0x8fce68];
    const DIRT: [u32; 4] = [0x866043, 0x79553a, 0x6b4a32, 0x96704f];
    const STONE: [u32; 4] = [0x7d7d7d, 0x6e6e6e, 0x8a8a8a, 0x747474];
    const LOG: [u32; 3] = [0x6b4f2b, 0x5c4323, 0x7a5a33];
    const LEAVES: [u32; 4] = [0x3f8f2a, 0x357a22, 0x4ea434, 0x2f6b1e];
    for (c, &gh) in ground.iter().enumerate() {
        let x = c as f32 * BLOCK;
        for k in 0..gh {
            let y = h - (gh - k) as f32 * BLOCK;
            if k == 0 {
                // Grass on top of dirt, the green dripping down the side.
                texture(&mut s, &mut r, x, y, &DIRT);
                for i in 0..4 {
                    let drip = 1 + (r.next() % 3) as i32;
                    for j in 0..drip {
                        let _ = write!(
                            s,
                            r##"<rect x="{:.0}" y="{:.0}" width="{px}" height="{px}" fill="#{:06x}"/>"##,
                            x + i as f32 * px,
                            y + j as f32 * px,
                            r.pick(&GRASS)
                        );
                    }
                }
            } else if k < 3 {
                texture(&mut s, &mut r, x, y, &DIRT);
            } else {
                texture(&mut s, &mut r, x, y, &STONE);
            }
        }
        // Now and then a tree.
        if c > 1 && c + 2 < ground.len() && r.f() < 0.09 {
            let top = h - gh as f32 * BLOCK;
            let trunk = 3 + (r.next() % 2) as i32;
            for k in 1..=trunk {
                texture(&mut s, &mut r, x, top - k as f32 * BLOCK, &LOG);
            }
            let crown = top - trunk as f32 * BLOCK;
            for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (0, -2), (-2, 0), (2, 0), (-1, -2), (1, -2), (0, 0)] {
                if (dx, dy) != (0, 0) {
                    texture(&mut s, &mut r, x + dx as f32 * BLOCK, crown + dy as f32 * BLOCK, &LEAVES);
                }
            }
        }
    }
    s
}

/// How many frames a second `scene` moves at, or None for one that holds still.
pub fn fps(scene: &str) -> Option<u64> {
    match scene {
        "nightcity" | "division" | "middleearth" | "bullworth" | "outerwilds" => Some(30),
        "blocks" => Some(12),
        _ => None,
    }
}

/// Whether `scene`'s picture and what moves in it lie under the page's darkening (a world in
/// daylight, which text must read over) rather than over it (a night scene, drawn dark, whose
/// lights should stay bright).
pub fn under_the_page(scene: &str) -> bool {
    scene == "blocks"
}

/// What moves in `scene` at `t` seconds, over an area `w` by `h`.
pub fn particles(scene: &str, t: f32, w: f32, h: f32) -> Vec<AnyElement> {
    match scene {
        "nightcity" => rain_and_glitches(t, w, h),
        "division" => snow(t, w, h),
        "middleearth" => embers(t, w, h),
        "bullworth" => paper_plane(t, w, h),
        "blocks" => clouds(t, w, h),
        "outerwilds" => campfire_and_sky(t, w, h),
        _ => Vec::new(),
    }
}

fn dot(x: f32, y: f32, size: f32, color: gpui::Hsla) -> AnyElement {
    div().absolute().left(px(x - size / 2.)).top(px(y - size / 2.)).size(px(size)).rounded_full().bg(color).into_any_element()
}

fn rgba(c: u32, a: f32) -> gpui::Hsla {
    gpui::Hsla::from(gpui::rgb(c)).opacity(a)
}

/// A picture made once from SVG, for things drawn many times.
fn picture(key: &'static str, svg: &'static str) -> Arc<gpui::Image> {
    thread_local! {
        static MADE: std::cell::RefCell<std::collections::HashMap<&'static str, Arc<gpui::Image>>> = Default::default();
    }
    MADE.with(|m| {
        m.borrow_mut()
            .entry(key)
            .or_insert_with(|| Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Svg, svg.as_bytes().to_vec())))
            .clone()
    })
}

/// Rain falling slantwise in the street's light, and every few seconds the picture tearing: bands
/// pulled sideways in cyan, red and yellow, as the game's glitches do.
fn rain_and_glitches(t: f32, w: f32, h: f32) -> Vec<AnyElement> {
    const SLANT: f32 = 0.22;
    let streak = picture(
        "rain",
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 32" width="16" height="64"><defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#bfefff" stop-opacity="0"/><stop offset="1" stop-color="#bfefff" stop-opacity="0.9"/></linearGradient></defs><path d="M7 0L1 32" stroke="url(#g)" stroke-width="1.4" stroke-linecap="round"/></svg>"##,
    );
    let n = ((w * h / 14_000.) as u32).clamp(20, 110);
    let mut out = Vec::with_capacity(n as usize + 8);
    for i in 0..n {
        let (len, speed) = (14. + hash(i, 1) * 16., 700. + hash(i, 2) * 500.);
        let span = h + len;
        let y = (hash(i, 3) * span + t * speed) % span - len;
        let x = (hash(i, 4) * (w + h * SLANT) - y * SLANT).rem_euclid(w + 40.) - 20.;
        out.push(
            gpui::img(streak.clone())
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(len / 4.))
                .h(px(len))
                .opacity(0.18 + hash(i, 5) * 0.22)
                .into_any_element(),
        );
    }
    // A glitch of a quarter of a second, every seven.
    const EVERY: f32 = 7.;
    let (burst, into) = ((t / EVERY).floor() as u32, t % EVERY);
    if into < 0.25 {
        let frame = (into * 30.) as u32;
        for k in 0..6 {
            let seed = burst * 97 + k * 13 + frame;
            let y = hash(seed, 6) * h;
            let bh = 2. + hash(seed, 7) * 16.;
            let x = (hash(seed, 8) - 0.3) * w * 0.4;
            let colour = [0x02d7f2, 0xff003c, 0xfcee0a][(seed % 3) as usize];
            out.push(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(w * (0.3 + hash(seed, 9) * 0.7)))
                    .h(px(bh))
                    .bg(rgba(colour, 0.22 + hash(seed, 10) * 0.25))
                    .into_any_element(),
            );
        }
    }
    out
}

/// Snow drifting down over the city.
fn snow(t: f32, w: f32, h: f32) -> Vec<AnyElement> {
    let n = ((w * h / 11_000.) as u32).clamp(30, 120);
    (0..n)
        .map(|i| {
            let size = 1.5 + hash(i, 1).powi(2) * 3.5;
            let speed = 18. + size * 11.;
            let span = h + 10.;
            let y = (hash(i, 2) * span + t * speed) % span - 5.;
            let sway = (t * (0.3 + hash(i, 3) * 0.5) + hash(i, 4) * 6.3).sin() * (8. + hash(i, 5) * 22.);
            let x = (hash(i, 6) * w + sway + t * 6.).rem_euclid(w);
            dot(x, y, size, rgba(0xf4f8ff, 0.35 + hash(i, 7) * 0.45))
        })
        .collect()
}

/// Embers rising from the valleys, flickering, gone before they reach the stars.
fn embers(t: f32, w: f32, h: f32) -> Vec<AnyElement> {
    let mut out = Vec::new();
    for i in 0..40u32 {
        let speed = 10. + hash(i, 1) * 22.;
        let span = h * 0.9;
        let rise = (hash(i, 2) * span + t * speed) % span;
        let y = h - rise;
        let x = (hash(i, 3) * w + (t * 0.4 + hash(i, 4) * 6.3).sin() * 26.).rem_euclid(w);
        let flicker = 0.55 + 0.45 * (t * (1.5 + hash(i, 5) * 2.5) + hash(i, 6) * 6.3).sin();
        let fade = (1. - rise / span).clamp(0., 1.);
        let a = flicker * fade;
        let size = 2. + hash(i, 7) * 2.5;
        out.push(dot(x, y, size * 4., rgba(0xffb347, 0.1 * a)));
        out.push(dot(x, y, size, rgba(0xffd98a, 0.85 * a)));
    }
    out
}

/// Now and then a paper plane glides across the page.
fn paper_plane(t: f32, w: f32, h: f32) -> Vec<AnyElement> {
    const EVERY: f32 = 22.;
    const FLIGHT: f32 = 7.;
    let (n, into) = ((t / EVERY).floor() as u32, t % EVERY);
    if into > FLIGHT {
        return Vec::new();
    }
    let plane = picture(
        "plane",
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 30" width="96" height="60"><path d="M2 14L46 2L18 28L16 18Z" fill="#fff" opacity="0.9"/><path d="M16 18L46 2L22 20Z" fill="#dfe6f2"/><path d="M16 18L18 28L22 20Z" fill="#b9c4d6"/></svg>"##,
    );
    let p = into / FLIGHT;
    let x = -60. + p * (w + 120.);
    let y = h * (0.2 + hash(n, 1) * 0.5) + (p * 7.).sin() * 24. - p * 40.;
    vec![gpui::img(plane).absolute().left(px(x)).top(px(y)).w(px(48.)).h(px(30.)).opacity(0.55).into_any_element()]
}

/// Square clouds drifting slowly across the sky.
fn clouds(t: f32, w: f32, h: f32) -> Vec<AnyElement> {
    let mut out = Vec::new();
    for i in 0..((w / 260.) as u32).clamp(3, 9) {
        let (cw, speed) = (BLOCK * (4. + (hash(i, 1) * 5.).floor()), 4. + hash(i, 2) * 6.);
        let x = (hash(i, 3) * (w + cw) + t * speed) % (w + cw) - cw;
        let y = (BLOCK * (1. + (hash(i, 4) * (h / BLOCK * 0.3)).floor())).min(h * 0.4);
        let cloud = div()
            .absolute()
            .left(px(x))
            .top(px(y))
            .child(div().absolute().top(px(0.)).left(px(BLOCK)).w(px(cw - BLOCK * 2.)).h(px(BLOCK)).bg(rgba(0xffffff, 0.9)))
            .child(div().absolute().top(px(BLOCK)).left(px(0.)).w(px(cw)).h(px(BLOCK)).bg(rgba(0xffffff, 0.9)))
            .child(div().absolute().top(px(BLOCK * 2.)).left(px(0.)).w(px(cw)).h(px(BLOCK / 2.)).bg(rgba(0xffffff, 0.9)));
        out.push(cloud.into_any_element());
    }
    out
}

/// The loop: every twenty-two minutes the Sun goes supernova.
const LOOP: f32 = 22. * 60.;
/// How long the end of the loop takes, from the Sun turning blue to its wave passing.
const SUPERNOVA: f32 = 9.;

/// Sparks from the campfire, stars twinkling, now and then a scout launched from the tower
/// across the sky, and at the end of each loop the Sun going supernova.
fn campfire_and_sky(t: f32, w: f32, h: f32) -> Vec<AnyElement> {
    let at = hearth(w, h);
    let mut out = Vec::new();
    for i in 0..26u32 {
        let (speed, span) = (26. + hash(i, 1) * 40., 140. + hash(i, 2) * 80.);
        let rise = (hash(i, 3) * span + t * speed) % span;
        let x = at.fire.0 + (hash(i, 4) - 0.5) * 22. + (t * 2. + hash(i, 5) * 6.3).sin() * 6. * rise / span;
        let a = (1. - rise / span) * (0.6 + 0.4 * (t * 9. + i as f32).sin());
        out.push(dot(x, at.fire.1 - 12. - rise, 2. + hash(i, 6) * 1.5, rgba(0xffc56a, 0.9 * a)));
    }
    for i in 0..34u32 {
        let a = 0.5 + 0.5 * (t * (0.8 + hash(i, 7) * 2.) + hash(i, 8) * 6.3).sin();
        out.push(dot(hash(i, 9) * w, hash(i, 10) * h * 0.7, 2. + hash(i, 11) * 1.5, rgba(0xeaf2ff, 0.15 + 0.6 * a * a)));
    }
    // A scout: every eighteen seconds, a second and a half from the tower up across the sky.
    let (n, into) = ((t / 18.).floor() as u32, t % 18.);
    if into < 1.5 {
        let p = into / 1.5;
        let (dx, dy) = (w * (0.2 + hash(n, 12) * 0.3), -at.tower.1 * (0.7 + hash(n, 13) * 0.25));
        for k in 0..8 {
            let q = (p - k as f32 * 0.02).max(0.);
            out.push(dot(at.tower.0 + dx * q, at.tower.1 + dy * q, 4. - k as f32 * 0.4, rgba(0xbfe8ff, 0.8 * (1. - k as f32 / 8.))));
        }
    }
    // The Sun turns blue, swells, and its wave goes out over everything.
    let left = LOOP - t % LOOP;
    if left < SUPERNOVA {
        let p = 1. - left / SUPERNOVA;
        let (sx, sy) = at.sun;
        out.push(dot(sx, sy, (30. + p * 60.) * 2., rgba(0x9fd6ff, 0.85)));
        if p > 0.45 {
            let q = (p - 0.45) / 0.55;
            let ring = q * w.max(h) * 2.4;
            out.push(
                div()
                    .absolute()
                    .left(px(sx - ring / 2.))
                    .top(px(sy - ring / 2.))
                    .size(px(ring))
                    .rounded_full()
                    .border(px(10.))
                    .border_color(rgba(0xd8f0ff, 0.5 * (1. - q)))
                    .bg(rgba(0x9fd6ff, 0.12 * (1. - q)))
                    .into_any_element(),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_picture_parses() {
        let renderer = gpui::SvgRenderer::new(std::sync::Arc::new(()));
        for (scene, _) in crate::theme::scenes() {
            if let Some(svg) = art(scene, 1240., 820.) {
                renderer.parse_svg(svg.as_bytes()).unwrap_or_else(|e| panic!("{scene}: {e}"));
            }
        }
    }

    #[test]
    fn the_sun_goes_supernova_at_the_end_of_each_loop() {
        let quiet = particles("outerwilds", 100., 1200., 800.).len();
        assert_eq!(particles("outerwilds", LOOP - SUPERNOVA - 1., 1200., 800.).len(), quiet);
        // The blue Sun, then the Sun and its wave.
        assert_eq!(particles("outerwilds", LOOP - 8., 1200., 800.).len(), quiet + 1);
        assert_eq!(particles("outerwilds", 2. * LOOP - 2., 1200., 800.).len(), quiet + 2);
    }

    #[test]
    fn the_same_picture_every_time() {
        assert_eq!(art("nightcity", 800., 600.), art("nightcity", 800., 600.));
    }
}
