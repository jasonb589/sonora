use gpui::accesskit::Role;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Div, ElementId, Entity, EntityId, FontWeight, Hsla, Pixels, Point,
    ScrollHandle, ScrollWheelEvent, SharedString, Stateful, WeakEntity, Window, div,
    linear_color_stop, linear_gradient, point, px,
};
use std::rc::Rc;

use music::{GenreItem, GenreSection};
use router::{Destination, navigate};
use state::Playback;
use ui::{
    ActiveTheme as _, Artwork, Button, Card, Deck, Glide, Mode, Skeleton, Text, heading, snapped,
};

use crate::shared::album_grid::CardGrid;
use crate::shared::cards;

const PLATE: Pixels = px(260.);
const LANES: usize = 5;
const ROWS: usize = 3;
const STEADY: Pixels = px(0.5);
const PENDING: usize = 3;
const RAIL_GAP: Pixels = px(16.);
const STACK_GAP: Pixels = px(32.);
const LANE_GAP: Pixels = px(8.);
const HEADING_GAP: Pixels = px(12.);
const LEADING: f32 = 1.4;
const HEADING: Pixels = px(140.);
/// How tall a genre plate stands, as a share of the row the theme gives a listed card: a wide
/// rectangle, so the picture a genre wears reads as a picture rather than a thumbnail.
const PLATE_RISE: f32 = 1.75;
/// The room between two genre plates, and between two rows of them.
const PLATE_GAP: Pixels = px(12.);
/// Where the shade under a plate's name starts, and how much of the theme's background it reaches
/// by its foot: the name reads over any picture while the picture still shows through.
const PLATE_SHADE_FROM: f32 = 0.45;
const PLATE_SHADE: f32 = 0.92;

pub(crate) struct Shelves {
    id: &'static str,
    host: EntityId,
    playback: Entity<Playback>,
    rails: Vec<(ScrollHandle, Glide)>,
}

impl Shelves {
    pub(crate) fn new(id: &'static str, host: EntityId, playback: Entity<Playback>) -> Self {
        Self {
            id,
            host,
            playback,
            rails: Vec::new(),
        }
    }

    fn tag(&self, kind: &str, place: usize) -> SharedString {
        SharedString::from(format!("{}-{kind}-{place}", self.id))
    }

    /// Draws loading rows through the page edges, with their headings inside the content inset.
    pub(crate) fn pending(&self, width: Pixels, cx: &App) -> Vec<AnyElement> {
        let theme = *cx.theme();
        let layout = CardGrid::layout(width);

        (0..PENDING)
            .map(|shelf| {
                div()
                    .flex()
                    .flex_col()
                    .mx(-theme.metrics.inset)
                    .gap_3()
                    .child(
                        Skeleton::new()
                            .mx(theme.metrics.inset)
                            .w(HEADING)
                            .h(theme.text(Text::Large))
                            .rounded(theme.radius),
                    )
                    .child(
                        div()
                            .flex()
                            .px(theme.metrics.inset)
                            .gap_4()
                            .overflow_hidden()
                            .children((0..=layout.columns).map(|place| {
                                Card::skeleton(self.tag("pending", shelf * 100 + place))
                                    .tile(layout.card)
                            })),
                    )
                    .into_any_element()
            })
            .collect()
    }

    pub(crate) fn reset(&mut self) {
        self.rails.clear();
    }

    /// Virtualizes full-width sections while keeping headings and lists inside the content inset.
    pub(crate) fn render(
        &mut self,
        sections: Rc<Vec<GenreSection>>,
        mode: Mode,
        width: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        while self.rails.len() < sections.len() {
            let mut glide = Glide::default();
            glide.watch(self.host);
            self.rails.push((ScrollHandle::new(), glide));
        }
        for (scroll, glide) in &self.rails {
            glide.sync(scroll);
        }

        let heights: Vec<Pixels> = sections
            .iter()
            .map(|section| self.height(section, mode, width, window, cx))
            .collect();
        let me = cx.entity().downgrade();

        let stack = Deck::new(self.tag("stack", 0))
            .rows(heights)
            .gap(STACK_GAP)
            .draw(move |place, window, cx| {
                let Some(view) = me.upgrade() else {
                    return div().into_any_element();
                };
                let Some(section) = sections.get(place) else {
                    return div().into_any_element();
                };
                let holder = view.downgrade();
                let shelves = view.read(cx);

                match mode {
                    Mode::Grid => shelves.rail(place, &sections, width, &holder, window, cx),
                    Mode::List => shelves.lane(place, section, width, window, cx),
                }
            });

        // Sections span the page so the deck's row clips do not cut off sideways cards.
        div()
            .relative()
            .mx(-cx.theme().metrics.inset)
            .child(stack)
            .into_any_element()
    }

    fn height(
        &self,
        section: &GenreSection,
        mode: Mode,
        width: Pixels,
        window: &Window,
        cx: &App,
    ) -> Pixels {
        let theme = *cx.theme();
        let head = head(window, cx) + HEADING_GAP;
        let body = match mode {
            Mode::Grid => Card::tile_height(CardGrid::layout(width).card, window, cx),
            Mode::List => {
                let lanes = lanes(width);
                let rows = section.items.len().min(lanes * ROWS).div_ceil(lanes);
                let row = snapped(theme.metrics.list_row, window);
                row * rows as f32 + LANE_GAP * rows.saturating_sub(1) as f32
            }
        };

        head + body
    }

    /// Lays out a section as inset columns without horizontal scrolling.
    fn lane(
        &self,
        place: usize,
        section: &GenreSection,
        width: Pixels,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let lanes = lanes(width);
        let total = section.items.len().min(lanes * ROWS);
        let cards = section
            .items
            .iter()
            .take(total)
            .enumerate()
            .map(|(index, item)| {
                self.card(place * 100 + index, item, None, cx)
                    .aria_position_in_set(index + 1)
                    .aria_size_of_set(total)
                    .into_any_element()
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .px(cx.theme().metrics.inset)
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_end()
                    .h(head(window, cx))
                    .child(shelf_heading(
                        self.tag("heading", place),
                        i18n::translate(&section.title),
                        cx,
                    )),
            )
            .child(spread(cards, lanes))
            .into_any_element()
    }

    /// Clips a section's cards at the page edges and keeps its heading aligned with the content.
    fn rail(
        &self,
        place: usize,
        sections: &Rc<Vec<GenreSection>>,
        width: Pixels,
        me: &WeakEntity<Self>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let Some(section) = sections.get(place) else {
            return div().into_any_element();
        };
        let layout = CardGrid::layout(width);
        let (handle, glide) = self.rails[place].clone();
        let crowded = section.items.len() > layout.columns;
        let feed = sections.clone();
        let drawn = me.clone();
        let card = layout.card;
        let tall = Card::tile_height(card, window, cx);

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .px(cx.theme().metrics.inset)
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .h(head(window, cx))
                    .group("rail-actions")
                    .child(shelf_heading(
                        self.tag("heading", place),
                        i18n::translate(&section.title),
                        cx,
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .invisible()
                            .group_hover("rail-actions", |style| style.visible())
                            .when(crowded, |this| {
                                this.child(self.arrows(place, &handle, &glide, me))
                            }),
                    ),
            )
            .child(
                div()
                    .id((self.id, place))
                    .w_full()
                    .px(cx.theme().metrics.inset)
                    .h(tall)
                    .overflow_x_scroll()
                    .restrict_scroll_to_axis()
                    .track_scroll(&handle)
                    .on_scroll_wheel({
                        let scroll = handle.clone();
                        let glide = glide.clone();
                        move |event: &ScrollWheelEvent, window, cx| {
                            ui::hold_swipe(event, &scroll, cx);
                            if !sideways(event) {
                                return;
                            }
                            glide.nudge(&scroll, window);
                        }
                    })
                    .child(
                        Deck::new(self.tag("rail", place))
                            .across()
                            .rows(section.items.iter().map(|_| card))
                            .gap(RAIL_GAP)
                            .draw(move |index, _, cx| {
                                let Some(view) = drawn.upgrade() else {
                                    return div().into_any_element();
                                };
                                let Some(items) = feed.get(place).map(|section| &section.items)
                                else {
                                    return div().into_any_element();
                                };
                                let Some(item) = items.get(index) else {
                                    return div().into_any_element();
                                };

                                view.read(cx)
                                    .card(place * 100 + index, item, Some(card), cx)
                                    .aria_position_in_set(index + 1)
                                    .aria_size_of_set(items.len())
                                    .into_any_element()
                            }),
                    ),
            )
            .into_any_element()
    }

    fn arrows(
        &self,
        place: usize,
        handle: &ScrollHandle,
        glide: &Glide,
        me: &WeakEntity<Self>,
    ) -> AnyElement {
        let at = glide.goal(handle).x;
        let reach = handle.max_offset().x;

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .child(
                self.arrow(self.tag("previous", place), false, handle, glide, me)
                    .disabled(at >= -STEADY),
            )
            .child(
                self.arrow(self.tag("next", place), true, handle, glide, me)
                    .disabled(reach > Pixels::ZERO && at <= STEADY - reach),
            )
            .into_any_element()
    }

    fn arrow(
        &self,
        id: impl Into<ElementId>,
        forward: bool,
        handle: &ScrollHandle,
        glide: &Glide,
        me: &WeakEntity<Self>,
    ) -> Button {
        let handle = handle.clone();
        let glide = glide.clone();
        let me = me.clone();

        Button::new(id)
            .small()
            .outline()
            .icon(match forward {
                true => "icons/chevron-right.svg",
                false => "icons/chevron-left.svg",
            })
            .tooltip(match forward {
                true => "common-next",
                false => "common-previous",
            })
            .on_click(move |_, window, cx| {
                slide(&handle, &glide, forward, window);
                me.update(cx, |_, cx| cx.notify()).ok();
            })
    }

    fn card(&self, id: usize, item: &GenreItem, tile: Option<Pixels>, cx: &App) -> Card {
        cards::item_card(slot("item", id), item, &self.playback, cx)
            .map(|card| dressed(card, tile, cx))
    }
}

/// One genre of the wall: its picture filling the plate and its name over the foot of it. A
/// genre the server keeps no picture for wears the colour its own name names, so the wall never
/// shows a hole where art is missing.
fn plate(
    id: impl Into<ElementId>,
    genre: &music::Genre,
    width: Pixels,
    height: Pixels,
    place: (usize, usize),
    cx: &App,
) -> AnyElement {
    let (index, total) = place;
    let theme = *cx.theme();
    let opened = SharedString::from(genre.id.clone());

    div()
        .id(id)
        .role(Role::Button)
        .aria_label(SharedString::from(genre.name.clone()))
        .aria_position_in_set(index + 1)
        .aria_size_of_set(total)
        .relative()
        .flex_none()
        .w_full()
        .h(height)
        .rounded(theme.radius)
        .overflow_hidden()
        .cursor_pointer()
        // The colour stands under the picture, so a plate whose art is still on its way is never
        // a white hole in the wall.
        .bg(theme.primary_of(named_hue(&genre.name)))
        .when_some(genre.cover.clone(), |this, cover| {
            this.child(Artwork::new(Some(cover)).size(width).wide(height))
        })
        .child(div().absolute().inset_0().bg(linear_gradient(
            180.,
            linear_color_stop(theme.background.opacity(0.), PLATE_SHADE_FROM),
            linear_color_stop(theme.background.opacity(PLATE_SHADE), 1.),
        )))
        .child(
            div()
                .absolute()
                .left(px(14.))
                .right(px(14.))
                .bottom(px(10.))
                .truncate()
                .text_size(theme.text(Text::Body))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child(SharedString::from(genre.name.clone())),
        )
        .on_click(move |_, _, cx| navigate(Destination::Genre(opened.clone()), cx))
        .into_any_element()
}

pub(crate) fn grid(
    id: &'static str,
    genres: Rc<Vec<music::Genre>>,
    width: Pixels,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let lanes = lanes(width);
    let gap = PLATE_GAP;
    let kept = width - gap * (lanes as f32 - 1.);
    let cell = kept / lanes as f32;
    let height = snapped(cx.theme().metrics.list_row * PLATE_RISE, window);
    let rows = genres.len().div_ceil(lanes);

    Deck::new(id)
        .rows((0..rows).map(|_| height))
        .gap(gap)
        .draw(move |row, _, cx| {
            let first = row * lanes;

            div()
                .flex()
                .w_full()
                .gap(gap)
                // Empty columns keep the final row's plates and their artwork the same width.
                .children((0..lanes).map(|column| {
                    let index = first + column;
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .when_some(genres.get(index), |this, genre| {
                            this.child(plate(
                                SharedString::from(format!("{id}-{}", genre.id)),
                                genre,
                                cell,
                                height,
                                (index, genres.len()),
                                cx,
                            ))
                        })
                }))
                .into_any_element()
        })
        .into_any_element()
}

/// The hue a genre with no picture wears: its own name picks it, so a genre keeps its colour from
/// one launch to the next and neighbours rarely match. Only the hue is used, since the theme pins
/// the weight every plate reads at.
fn named_hue(name: &str) -> Hsla {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }

    Hsla {
        h: (hash % 360) as f32 / 360.,
        s: 0.5,
        l: 0.5,
        a: 1.,
    }
}

/// How many columns a grid of genre plates `width` wide lays out.
pub(crate) fn lanes(width: Pixels) -> usize {
    ((width / PLATE).floor().max(1.) as usize).min(LANES)
}

fn spread(cards: Vec<AnyElement>, lanes: usize) -> Div {
    let mut columns: Vec<Vec<AnyElement>> = (0..lanes).map(|_| Vec::new()).collect();
    for (place, card) in cards.into_iter().enumerate() {
        columns[place % lanes].push(card);
    }

    div()
        .flex()
        .w_full()
        .gap_2()
        .children(columns.into_iter().map(|column| {
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .flex_col()
                .gap_2()
                .children(column)
        }))
}

fn head(window: &Window, cx: &App) -> Pixels {
    let theme = *cx.theme();

    snapped(theme.text(Text::Title) * LEADING, window).max(theme.metrics.control_small)
}

fn dressed(card: Card, tile: Option<Pixels>, cx: &App) -> Card {
    match tile {
        Some(width) => card.tile(width).flat(),
        None => card.bg(cx.theme().secondary),
    }
}

fn slide(handle: &ScrollHandle, glide: &Glide, forward: bool, window: &mut Window) {
    let page = handle.bounds().size.width;
    let at = glide.goal(handle);
    let next = match forward {
        true => at.x - page,
        false => at.x + page,
    };

    glide.aim(handle, point(next, at.y), window);
}

/// One horizontal card rail outside a shelf page: a heading with paging arrows over a
/// sideways scroller. Artist and album pages draw their recommendations with these, so
/// neither rebuilds the scroll plumbing a shelf keeps for its sections.
pub(crate) struct Rail {
    scroll: ScrollHandle,
    glide: Glide,
}

impl Rail {
    /// A rail that reports its scrolls to `host`, the page holding it.
    pub(crate) fn new(host: EntityId) -> Self {
        let mut glide = Glide::default();
        glide.watch(host);
        Self {
            scroll: ScrollHandle::new(),
            glide,
        }
    }

    /// Catches the glide up with its scroller before the frame draws it.
    pub(crate) fn sync(&self) {
        self.glide.sync(&self.scroll);
    }

    /// Puts the rail back at its first card at once, for when it starts showing other cards.
    pub(crate) fn rewind(&self) {
        self.glide.jump(&self.scroll, Point::default());
    }

    /// The rail under its heading: the title with paging arrows beside it while the cards
    /// overflow, the kind pills when the rail has kinds to narrow by, and the cards in a
    /// sideways scroller. `notify` repaints the page holding the rail, which the arrows
    /// need to refresh themselves.
    pub(crate) fn render(
        &self,
        spec: RailSpec,
        window: &Window,
        cx: &mut App,
        notify: &Rc<dyn Fn(&mut App)>,
        draw: impl Fn(usize, &mut Window, &mut App) -> AnyElement + 'static,
    ) -> AnyElement {
        let RailSpec {
            tag,
            place,
            title,
            count,
            tile,
            columns,
            tabs,
        } = spec;
        let tall = Card::tile_height(tile, window, cx);
        let inset = cx.theme().metrics.inset;
        let scrolled = self.scroll.clone();
        let glided = self.glide.clone();
        let arrows = match count > columns {
            true => Some(arrows(tag, place, &self.scroll, &self.glide, notify)),
            false => None,
        };

        div()
            .flex()
            .flex_col()
            .mx(-inset)
            .gap_3()
            .child(
                div()
                    .flex()
                    .px(inset)
                    .items_end()
                    .justify_between()
                    .gap_4()
                    .h(head(window, cx))
                    .child(shelf_heading(
                        SharedString::from(format!("{tag}-heading-{place}")),
                        title,
                        cx,
                    ))
                    .children(arrows),
            )
            .children(tabs.map(|tabs| div().px(inset).child(tabs)))
            .child(
                div()
                    .id((tag, place))
                    .w_full()
                    .px(inset)
                    .h(tall)
                    .overflow_x_scroll()
                    .restrict_scroll_to_axis()
                    .track_scroll(&self.scroll)
                    .on_scroll_wheel(move |event: &ScrollWheelEvent, window, cx| {
                        ui::hold_swipe(event, &scrolled, cx);
                        if !sideways(event) {
                            return;
                        }
                        glided.nudge(&scrolled, window);
                    })
                    .child(
                        Deck::new(SharedString::from(format!("{tag}-rail-{place}")))
                            .across()
                            .rows(std::iter::repeat_n(tile, count))
                            .gap(RAIL_GAP)
                            .draw(draw),
                    ),
            )
            .into_any_element()
    }

    /// The rail while its cards are still on their way: the heading over skeleton tiles.
    pub(crate) fn pending(
        title: SharedString,
        tile: Pixels,
        columns: usize,
        window: &Window,
        cx: &mut App,
    ) -> AnyElement {
        let tall = Card::tile_height(tile, window, cx);
        let inset = cx.theme().metrics.inset;

        div()
            .flex()
            .flex_col()
            .mx(-inset)
            .gap_3()
            .child(
                div()
                    .flex()
                    .px(inset)
                    .items_end()
                    .h(head(window, cx))
                    .child(shelf_heading(
                        SharedString::from(format!("rail-pending-heading-{title}")),
                        title,
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .w_full()
                    .px(inset)
                    .h(tall)
                    .gap(RAIL_GAP)
                    .overflow_hidden()
                    .children((0..=columns).map(|place| {
                        Card::skeleton(("rail-pending", place))
                            .tile(tile)
                            .into_any_element()
                    })),
            )
            .into_any_element()
    }
}

/// What a rail draws: its heading and the cards behind it. `tile` is how wide one card
/// stands and `columns` how many fit across, which is what decides whether the paging
/// arrows show. `tabs` is the tab row, drawn between the heading and the cards.
pub(crate) struct RailSpec {
    pub tag: &'static str,
    pub place: usize,
    pub title: SharedString,
    pub count: usize,
    pub tile: Pixels,
    pub columns: usize,
    pub tabs: Option<AnyElement>,
}

/// The paging arrows of a rail head, which is what tells a crowded rail from a short one.
fn arrows(
    tag: &'static str,
    place: usize,
    scroll: &ScrollHandle,
    glide: &Glide,
    notify: &Rc<dyn Fn(&mut App)>,
) -> AnyElement {
    let at = glide.goal(scroll).x;
    let reach = scroll.max_offset().x;

    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .child(
            arrow(
                SharedString::from(format!("{tag}-previous-{place}")),
                false,
                scroll,
                glide,
                notify,
            )
            .disabled(at >= -STEADY),
        )
        .child(
            arrow(
                SharedString::from(format!("{tag}-next-{place}")),
                true,
                scroll,
                glide,
                notify,
            )
            .disabled(reach > Pixels::ZERO && at <= STEADY - reach),
        )
        .into_any_element()
}

fn arrow(
    id: SharedString,
    forward: bool,
    scroll: &ScrollHandle,
    glide: &Glide,
    notify: &Rc<dyn Fn(&mut App)>,
) -> Button {
    let scroll = scroll.clone();
    let glide = glide.clone();
    let notify = notify.clone();

    Button::new(id)
        .small()
        .outline()
        .icon(match forward {
            true => "icons/chevron-right.svg",
            false => "icons/chevron-left.svg",
        })
        .tooltip(match forward {
            true => "common-next",
            false => "common-previous",
        })
        .on_click(move |_, window, cx| {
            slide(&scroll, &glide, forward, window);
            notify(cx);
        })
}

fn slot(kind: &'static str, place: usize) -> ElementId {
    ElementId::NamedInteger(SharedString::new_static(kind), place as u64)
}

/// Whether a wheel event should glide a rail. Only a notched wheel moving sideways does, since a
/// trackpad scrolls the rail itself and a vertical notch belongs to the page.
fn sideways(event: &ScrollWheelEvent) -> bool {
    !event.delta.precise() && event.delta.pixel_delta(px(1.)).x != Pixels::ZERO
}

/// A shelf or rail title as a level two heading, so screen readers can jump from one shelf to
/// the next. `id` has to be unique on the page.
fn shelf_heading(id: impl Into<ElementId>, title: SharedString, cx: &App) -> Stateful<Div> {
    heading(title.clone(), cx)
        .id(id)
        .role(Role::Heading)
        .aria_level(2)
        .aria_label(title)
}
