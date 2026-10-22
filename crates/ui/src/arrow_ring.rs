use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, ElementId, Hsla, PathBuilder, Pixels,
    Point, Window, canvas, point, px,
};

use crate::motion;
use crate::theme::ActiveTheme as _;

/// The mark's size, the same as a button's icon.
const SIZE: Pixels = px(16.);
/// The grid the mark is drawn on, Lucide's `circle-arrow-down`.
const GRID: f32 = 24.;
const STROKE: f32 = 2.;
const RADIUS: f32 = 10.;
/// How many segments a whole ring is drawn with.
const SEGMENTS: f32 = 64.;
/// How long one turn of the ring takes while the amount done is unknown.
const TURN: Duration = Duration::from_millis(900);
/// How much of the ring turns while the amount done is unknown.
const SWEEP: f32 = 0.25;
/// How strongly the empty part of the ring shows.
const TRACK: f32 = 0.3;
/// How quickly the drawn ring closes on the amount done, as the time it takes to cover most of
/// the way. It is about one redraw of a running download, so steps between redraws blend into
/// one movement.
const CATCH_UP: f32 = 0.22;
/// How near the amount done the drawn ring has to be to stop asking for frames.
const SETTLED: f32 = 0.0005;

/// A down arrow in a ring that fills clockwise from the top as work goes, and closes into
/// Lucide's `circle-arrow-down` when it is done. It is drawn rather than loaded, so it looks the
/// same whatever icon pack is active. With the amount unknown, a short arc turns instead, and
/// holds still when the system asks for less motion.
#[derive(IntoElement)]
pub struct ArrowRing {
    id: ElementId,
    done: Option<f32>,
    color: Option<Hsla>,
}

impl ArrowRing {
    /// A ring `done` of the way round, from 0 to 1, or turning when `None`.
    pub fn new(id: impl Into<ElementId>, done: Option<f32>) -> Self {
        Self {
            id: id.into(),
            done,
            color: None,
        }
    }

    /// The colour of the arrow and of the filled part of the ring. The muted foreground by
    /// default.
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl RenderOnce for ArrowRing {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let color = self.color.unwrap_or(theme.muted_foreground);
        let track = theme.muted_foreground.opacity(TRACK);
        let animates = motion::animates(cx);
        match (self.done, animates) {
            (Some(done), _) => {
                let fill = window.use_keyed_state((self.id, "fill"), cx, |_, _| Fill::new(done));
                let drawn = fill.update(cx, |fill, _| fill.toward(done, animates));
                if drawn != done {
                    window.request_animation_frame();
                }
                mark(Ring::Filled(drawn), color, track)
            }
            (None, false) => mark(Ring::Turning(0.), color, track),
            (None, true) => mark(Ring::Turning(0.), color, track)
                .with_animation(self.id, Animation::new(TURN).repeat(), move |_, turned| {
                    mark(Ring::Turning(turned), color, track)
                })
                .into_any_element(),
        }
    }
}

/// How much of the ring is drawn while the amount done moves. A rise glides, so the steps a
/// download reports in turn into one smooth fill. A fall jumps, since it means another download
/// or a reset rather than progress.
struct Fill {
    drawn: f32,
    at: Instant,
}

impl Fill {
    fn new(done: f32) -> Self {
        Self {
            drawn: done,
            at: Instant::now(),
        }
    }

    /// Moves the drawn amount on by the time since the last frame and returns it. It lands on
    /// `done` once close enough, at once when nothing may animate.
    fn toward(&mut self, done: f32, animates: bool) -> f32 {
        let now = Instant::now();
        let elapsed = now.duration_since(self.at).as_secs_f32();
        self.at = now;
        if !animates || done < self.drawn {
            self.drawn = done;
            return done;
        }
        self.drawn += (done - self.drawn) * (1. - (-elapsed / CATCH_UP).exp());
        if done - self.drawn < SETTLED {
            self.drawn = done;
        }
        self.drawn
    }
}

/// How much of the ring is drawn.
#[derive(Clone, Copy)]
enum Ring {
    /// From the top, this far round clockwise.
    Filled(f32),
    /// A short arc starting this far round.
    Turning(f32),
}

fn mark(ring: Ring, color: Hsla, track: Hsla) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| paint(bounds, ring, color, track, window),
    )
    .size(SIZE)
    .flex_none()
    .into_any_element()
}

fn paint(bounds: Bounds<Pixels>, ring: Ring, color: Hsla, track: Hsla, window: &mut Window) {
    let scale = bounds.size.width / px(GRID);
    let width = px(STROKE * scale);
    let at = |x: f32, y: f32| {
        point(
            bounds.origin.x + px(x * scale),
            bounds.origin.y + px(y * scale),
        )
    };
    let (start, sweep) = match ring {
        Ring::Filled(done) => (0., done.clamp(0., 1.)),
        Ring::Turning(turned) => (turned, SWEEP),
    };
    if sweep < 1. {
        arc(&at, 0., 1., width, track, window);
    }
    if sweep > 0. {
        arc(&at, start, sweep, width, color, window);
    }
    stroke(&[at(12., 8.), at(12., 16.)], width, color, window);
    stroke(
        &[at(8., 12.), at(12., 16.), at(16., 12.)],
        width,
        color,
        window,
    );
}

/// Strokes `sweep` of the ring clockwise from `start`, both as fractions of a turn from the top.
fn arc(
    at: &impl Fn(f32, f32) -> Point<Pixels>,
    start: f32,
    sweep: f32,
    width: Pixels,
    color: Hsla,
    window: &mut Window,
) {
    let whole = sweep >= 1.;
    let steps = (SEGMENTS * sweep).ceil().max(2.) as usize;
    let points: Vec<Point<Pixels>> = (0..=steps)
        .map(|step| {
            let angle = (start + sweep * step as f32 / steps as f32) * TAU - FRAC_PI_2;
            at(12. + RADIUS * angle.cos(), 12. + RADIUS * angle.sin())
        })
        .collect();
    match whole {
        true => {
            let mut path = PathBuilder::stroke(width);
            path.add_polygon(&points[..steps], true);
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        }
        false => stroke(&points, width, color, window),
    }
}

/// Strokes an open line through `points` with round ends and joins, which gpui's stroke lacks,
/// so a dot is laid over each. An arc's joints are too shallow to need one, so a long line gets
/// dots at its ends alone.
fn stroke(points: &[Point<Pixels>], width: Pixels, color: Hsla, window: &mut Window) {
    let mut path = PathBuilder::stroke(width);
    path.add_polygon(points, false);
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
    let dotted: Vec<&Point<Pixels>> = match points.len() > 3 {
        true => points.first().into_iter().chain(points.last()).collect(),
        false => points.iter().collect(),
    };
    for point in dotted {
        dot(*point, width / 2., color, window);
    }
}

fn dot(center: Point<Pixels>, radius: Pixels, color: Hsla, window: &mut Window) {
    let points: Vec<Point<Pixels>> = (0..12)
        .map(|step| {
            let angle = step as f32 / 12. * TAU;
            point(
                center.x + radius * angle.cos(),
                center.y + radius * angle.sin(),
            )
        })
        .collect();
    let mut path = PathBuilder::fill();
    path.add_polygon(&points, true);
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}
