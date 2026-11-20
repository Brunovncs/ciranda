//! The app's icon, which the colour themes come with one of each (Settings → Appearance), as
//! Discord's app icons do: Ciranda's five bars on the theme's own picture. Drawn here as SVG on a
//! 256 grid; `crate::tray` puts it on the window, the taskbar and the notification area.

use std::fmt::Write;

/// Every icon: the name settings store (empty for Ciranda's own) and the one people see.
pub fn all() -> [(&'static str, &'static str); 8] {
    let [a, b, c, d, e, f, g] = crate::theme::scenes();
    [("", "Ciranda"), a, b, c, d, e, f, g]
}

/// The icon `key` names, or None for one this version does not know.
pub fn svg(key: &str) -> Option<String> {
    let body = match key {
        "" => format!("{}{}", fill(&[(0x5b8cff, 0.), (0xa78bfa, 1.)], (0., 0., 1., 1.)), bars(&Bars::default())),
        "vicecity" => vice_city(),
        "middleearth" => middle_earth(),
        "bullworth" => bullworth(),
        "division" => division(),
        "blocks" => blocks(),
        "nightcity" => night_city(),
        "outerwilds" => outer_wilds(),
        _ => return None,
    };
    let tile = match key {
        "blocks" => r#"<rect width="256" height="256" rx="28"/>"#.to_string(),
        // The game's panels: corners cut, not rounded.
        "nightcity" => r#"<path d="M52 0H256V204L204 256H0V52Z"/>"#.to_string(),
        _ => r#"<rect width="256" height="256" rx="56"/>"#.to_string(),
    };
    Some(format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" width="256" height="256"><defs><clipPath id="tile">{tile}</clipPath></defs><g clip-path="url(#tile)">{body}</g></svg>"#
    ))
}

/// The tile's background: a gradient from `(x1, y1)` to `(x2, y2)` of the tile.
fn fill(stops: &[(u32, f32)], (x1, y1, x2, y2): (f32, f32, f32, f32)) -> String {
    let mut s = format!(r#"<defs><linearGradient id="bg" x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}">"#);
    for (c, at) in stops {
        let _ = write!(s, r##"<stop offset="{at}" stop-color="#{c:06x}"/>"##);
    }
    s + r#"</linearGradient></defs><rect width="256" height="256" fill="url(#bg)"/>"#
}

/// The five bars, as the app icon draws them.
struct Bars {
    color: &'static str,
    heights: [f32; 5],
    width: f32,
    round: bool,
    /// Moved by this much, for the copies behind a glitch.
    shift: (f32, f32),
    opacity: f32,
}

impl Default for Bars {
    fn default() -> Bars {
        Bars { color: "#fff", heights: [0.18, 0.36, 0.52, 0.36, 0.18], width: 21., round: true, shift: (0., 0.), opacity: 1. }
    }
}

fn bars(b: &Bars) -> String {
    let mut s = String::new();
    for (i, h) in b.heights.iter().enumerate() {
        let (cx, h) = (128. + (i as f32 - 2.) * 35.75 + b.shift.0, h * 256.);
        let _ = write!(
            s,
            r#"<rect x="{x:.2}" y="{y:.2}" width="{w}" height="{h:.2}" rx="{r}" fill="{c}" opacity="{o}"/>"#,
            x = cx - b.width / 2.,
            y = 128. - h / 2. + b.shift.1,
            w = b.width,
            r = if b.round { b.width / 2. } else { 0. },
            c = b.color,
            o = b.opacity,
        );
    }
    s
}

/// White bars with a soft light around them.
fn glowing_bars() -> String {
    [(37., 0.1), (29., 0.14)].iter().map(|&(width, opacity)| bars(&Bars { width, opacity, ..Bars::default() })).collect::<String>()
        + &bars(&Bars::default())
}

/// A palm frond leaving `(x, y)` at `angle` degrees (0 to the right, turning down), `length`
/// long, bending by `droop` degrees along it, with leaflets up to `leaf` long on both sides.
fn frond(s: &mut String, (x, y): (f32, f32), angle: f32, length: f32, droop: f32, leaf: f32) {
    const N: usize = 16;
    let mut spine = Vec::with_capacity(N + 1);
    let (mut x, mut y, mut a) = (x, y, angle.to_radians());
    spine.push((x, y, a));
    for _ in 0..N {
        a += droop.to_radians() / N as f32;
        x += a.cos() * length / N as f32;
        y += a.sin() * length / N as f32;
        spine.push((x, y, a));
    }
    for i in 1..=N {
        let ((px, py, _), (x, y, a)) = (spine[i - 1], spine[i]);
        let t = i as f32 / N as f32;
        let l = leaf * (1. - 0.75 * t) * (0.6 + 0.4 * (std::f32::consts::PI * (t * 1.4).min(1.)).sin());
        for side in [-1., 1.] {
            let la = a + side * (62. - 20. * t).to_radians();
            let (tx, ty) = (x + la.cos() * l + a.cos() * l * 0.35, y + la.sin() * l + a.sin() * l * 0.35);
            let _ = write!(s, "M{px:.1} {py:.1}L{x:.1} {y:.1}L{tx:.1} {ty:.1}Z");
        }
    }
    // The stem, narrowing to the tip.
    let edge = |side: f32| -> Vec<(f32, f32)> {
        spine
            .iter()
            .enumerate()
            .map(|(i, &(x, y, a))| {
                let w = 2.2 * (1. - i as f32 / spine.len() as f32) + 0.4;
                let n = a + side * std::f32::consts::FRAC_PI_2;
                (x + n.cos() * w, y + n.sin() * w)
            })
            .collect()
    };
    let outline: Vec<_> = edge(-1.).into_iter().chain(edge(1.).into_iter().rev()).collect();
    for (i, (x, y)) in outline.iter().enumerate() {
        let _ = write!(s, "{}{x:.1} {y:.1}", if i == 0 { "M" } else { "L" });
    }
    s.push('Z');
}

fn palm(at: (f32, f32), fronds: &[(f32, f32, f32, f32)]) -> String {
    let mut s = String::new();
    for &(angle, length, droop, leaf) in fronds {
        frond(&mut s, at, angle, length, droop, leaf);
    }
    s
}

/// Discord's: the campaign's sunset, palms in the corners and the mark glowing in the middle.
fn vice_city() -> String {
    let top = palm(
        (240., 10.),
        &[
            (190., 165., -40., 46.),
            (165., 160., -34., 44.),
            (142., 140., -28., 40.),
            (212., 140., -30., 40.),
            (120., 110., -20., 34.),
            (232., 110., -10., 30.),
        ],
    );
    let bottom = palm(
        (-10., 262.),
        &[(-40., 150., 40., 40.), (-16., 140., 34., 40.), (-64., 130., 30., 36.), (6., 100., 26., 30.), (-88., 96., 20., 28.)],
    );
    fill(&[(0x5060c3, 0.), (0x9a56c3, 0.3333), (0xe3809c, 0.6667), (0xe8a252, 1.)], (0., 0., 1., 1.))
        + &format!(r##"<path d="{top}" fill="#6d1fa8" opacity="0.72"/><path d="{bottom}" fill="#b8306e" opacity="0.66"/>"##)
        + &glowing_bars()
}

/// A gold ring in a dark forest.
fn middle_earth() -> String {
    let ring: String = [(30., "#f3c86a", 0.1), (13., "url(#gold)", 1.), (2.5, "#fff3c4", 0.7)]
        .iter()
        .map(|(w, c, o)| format!(r#"<circle cx="128" cy="128" r="98" fill="none" stroke="{c}" stroke-width="{w}" opacity="{o}"/>"#))
        .collect();
    fill(&[(0x0b1f17, 0.), (0x1f4a35, 0.45), (0x8a6a2c, 0.85), (0xe2b85c, 1.)], (0.2, 0., 0.8, 1.))
        + r##"<defs><linearGradient id="gold" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#fff0b0"/><stop offset="0.45" stop-color="#e2b85c"/><stop offset="1" stop-color="#9c6a1e"/></linearGradient></defs>"##
        + &ring
        + &bars(&Bars { color: "#fff6dc", heights: [0.13, 0.26, 0.38, 0.26, 0.13], width: 17., ..Bars::default() })
}

/// A page of a school notebook, underlined.
fn bullworth() -> String {
    let lines: String =
        (34..256).step_by(26).map(|y| format!(r##"<rect x="0" y="{y}" width="256" height="3" fill="#fff" opacity="0.13"/>"##)).collect();
    fill(&[(0x1b2a55, 0.), (0x6b1f3a, 0.45), (0xc0392b, 0.78), (0xe8a33d, 1.)], (0., 0., 1., 1.))
        + &lines
        + r##"<rect x="38" y="0" width="4" height="256" fill="#ff8a8a" opacity="0.55"/>"##
        + &bars(&Bars::default())
        + r##"<path d="M52 214 C 92 200, 150 222, 206 204" fill="none" stroke="#fff" stroke-width="7" stroke-linecap="round"/>"##
}

/// An agent's watch: an orange ring in segments on the dark.
fn division() -> String {
    fill(&[(0x2a2f36, 0.), (0x16191e, 0.6), (0x0d0f12, 1.)], (0., 0., 1., 1.))
        + r##"<circle cx="128" cy="128" r="100" fill="none" stroke="#ff6d10" stroke-width="10" stroke-dasharray="44 8.36" transform="rotate(-86 128 128)"/><circle cx="128" cy="128" r="84" fill="none" stroke="#ff6d10" stroke-width="2" opacity="0.5"/>"##
        + &bars(&Bars { color: "#ff6d10", heights: [0.12, 0.24, 0.36, 0.24, 0.12], width: 16., ..Bars::default() })
}

/// A block of grass, sixteen pixels by sixteen, and the bars in pixels too.
fn blocks() -> String {
    const DIRT: [&str; 5] = ["#866043", "#79553a", "#6b4a32", "#96704f", "#5d402b"];
    const GRASS: [&str; 5] = ["#7cbd5a", "#6aa84f", "#5f9b3a", "#8fce68", "#4c8a2b"];
    // The same speckles every time.
    let mut seed = 0x2545_f491_u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut s = String::new();
    for gx in 0..16 {
        let edge = 4 + (next() % 100 < 45) as u32 + (next() % 100 < 20) as u32;
        for gy in 0..16 {
            let pal = if gy < edge { &GRASS } else { &DIRT };
            let c = pal[(next() % 5) as usize];
            let _ = write!(s, r#"<rect x="{}" y="{}" width="16" height="16" fill="{c}"/>"#, gx * 16, gy * 16);
        }
    }
    for (i, h) in [48, 96, 128, 96, 48].iter().enumerate() {
        let x = 32 + i * 40;
        let y = 128 - h / 2;
        let _ = write!(
            s,
            r##"<rect x="{}" y="{}" width="16" height="{h}" fill="#000" opacity="0.35"/><rect x="{x}" y="{y}" width="16" height="{h}" fill="#fff"/>"##,
            x + 6,
            y + 6
        );
    }
    s
}

/// A sign's yellow, the bars torn into cyan and red.
fn night_city() -> String {
    let square = |color, shift| bars(&Bars { color, round: false, width: 19., shift, ..Bars::default() });
    // The yellow panel, a black band cut along its top with a readout in it, the bars torn into
    // cyan and red, hazard stripes and a red light in the corner.
    let mut s = String::from(r##"<rect width="256" height="256" fill="#fcee0a"/><path d="M52 0H256V34H70L58 46H0V52Z" fill="#0a0a0f"/>"##);
    for (i, w) in [6., 2., 4., 2., 8., 3., 2., 5., 2., 3.].iter().enumerate() {
        let _ = write!(s, r##"<rect x="{:.0}" y="12" width="{w}" height="12" fill="#fcee0a" opacity="0.85"/>"##, 150. + i as f32 * 9.);
    }
    s += r##"<rect x="80" y="14" width="44" height="8" fill="#02d7f2"/>"##;
    s += &square("#02d7f2", (-6., 2.));
    s += &square("#ff003c", (6., -2.));
    s += &square("#0a0a0f", (0., 4.));
    // Two slices of the bars pulled sideways.
    s += r##"<rect x="40" y="112" width="120" height="7" fill="#fcee0a"/><rect x="66" y="112" width="70" height="7" fill="#0a0a0f"/><rect x="104" y="150" width="110" height="5" fill="#02d7f2" opacity="0.9"/>"##;
    s += r##"<path d="M0 204L52 256H74L0 182Z M0 160L96 256H118L0 138Z" fill="#0a0a0f"/><rect x="214" y="214" width="12" height="12" fill="#ff003c" transform="rotate(45 220 220)"/>"##;
    s
}

/// Outer Wilds: the signalscope listening over Timber Hearth. The bars are its reading, the Sun
/// burns in the corner, a Nomai spiral in the dark and pines on the planet's curve below.
fn outer_wilds() -> String {
    let mut s = fill(&[(0x070b1e, 0.), (0x15183a, 0.55), (0x3a2240, 1.)], (0., 0., 0.6, 1.));
    for (x, y, r, o) in [
        (30., 40., 1.6, 0.8),
        (70., 22., 1.1, 0.6),
        (112., 52., 1.3, 0.7),
        (210., 120., 1.2, 0.5),
        (36., 112., 1., 0.5),
        (150., 18., 1.4, 0.8),
        (226., 30., 1., 0.4),
    ] {
        let _ = write!(s, r##"<circle cx="{x}" cy="{y}" r="{r}" fill="#eaf2ff" opacity="{o}"/>"##);
    }
    s += r##"<ellipse cx="200" cy="52" rx="120" ry="34" fill="none" stroke="#9fd9ff" stroke-width="1.5" opacity="0.25" transform="rotate(-14 200 52)"/>"##;
    s += r##"<circle cx="206" cy="50" r="64" fill="#ef7b39" opacity="0.18"/><circle cx="206" cy="50" r="38" fill="#ef7b39" opacity="0.3"/><circle cx="206" cy="50" r="22" fill="#ffd27a"/>"##;
    s += r##"<path d="M44 52m-20 0a20 20 0 1 1 34 14a14 14 0 1 1 -22 -12a8 8 0 1 1 12 6" fill="none" stroke="#9fb4ff" stroke-width="3.5" stroke-linecap="round" opacity="0.6"/>"##;
    // The planet's curve, and pines along it.
    s += r##"<circle cx="128" cy="470" r="250" fill="#14302a"/><circle cx="128" cy="470" r="250" fill="none" stroke="#2f5a46" stroke-width="3"/>"##;
    let mut pines = String::new();
    for x in [18., 40., 66., 190., 214., 238.] {
        let base = 470. - (250f32.powi(2) - (x - 128f32).powi(2)).sqrt() + 3.;
        let _ = write!(pines, "M{x} {:.1}L{:.1} {base:.1}L{:.1} {base:.1}Z", base - 30., x - 9., x + 9.);
    }
    let _ = write!(s, r##"<path d="{pines}" fill="#0a1a14"/>"##);
    // The reading, glowing in the campfire's colour.
    let reading = |width: f32, colour, opacity| bars(&Bars { color: colour, width, opacity, shift: (0., -16.), ..Bars::default() });
    s + &reading(33., "#ef7b39", 0.22) + &reading(21., "#fff4e2", 1.)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_draws() {
        let renderer = gpui::SvgRenderer::new(std::sync::Arc::new(()));
        for (key, _) in all() {
            let svg = svg(key).unwrap_or_else(|| panic!("no icon for {key:?}"));
            let tree = renderer.parse_svg(svg.as_bytes()).unwrap_or_else(|e| panic!("{key:?}: {e}"));
            let px = gpui::DevicePixels(64);
            let image = renderer.render_parsed(&tree, gpui::SvgSize::ExactSize(gpui::size(px, px))).expect("drawn");
            let bytes = image.as_bytes(0).expect("pixels");
            // Opaque in the middle, see-through in the rounded corner.
            let at = |x: usize, y: usize| bytes[(y * 64 + x) * 4 + 3];
            assert_eq!(at(32, 32), 255, "{key:?}");
            assert_eq!(at(0, 0), 0, "{key:?}");
        }
        assert!(svg("glitter").is_none());
    }
}
