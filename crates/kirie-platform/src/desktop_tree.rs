//! Working out where a wallpaper belongs among Explorer's desktop windows.
//!
//! Windows has had two shapes of desktop since Windows 7, and a wallpaper goes
//! somewhere different in each:
//!
//! - **Classic** (Windows 10, Windows 11 up to 23H2). Message `0x052C` splits
//!   Progman into two top-level `WorkerW` windows: one holding
//!   `SHELLDLL_DefView`, which is the icons, and an empty one behind it. The
//!   empty one is ours, and our windows are children of it.
//! - **Raised** (Windows 11 24H2 servicing and 25H2). Progman keeps the icons
//!   as a direct child, is made with `WS_EX_NOREDIRECTIONBITMAP`, and `0x052C`
//!   makes a `WorkerW` *child* of Progman, below the icons, which draws the
//!   Windows wallpaper. Nothing is ours there: Microsoft's advice, as Lively
//!   quotes it, is to add a layered child of Progman of our own, below
//!   `SHELLDLL_DefView` and above that `WorkerW`.
//!
//! The decision is kept apart from the Win32 calls that gather what it looks
//! at, so that it can be tested on any machine. `W` is a window handle on
//! Windows and a plain number in the tests.

/// The class of the window holding the desktop icons.
pub(crate) const ICONS: &str = "SHELLDLL_DefView";

/// The class of the windows `0x052C` makes.
pub(crate) const WORKER: &str = "WorkerW";

/// One window, as much of it as the decision needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Seen<W> {
    pub(crate) window: W,
    pub(crate) class: String,
    /// Whether it has a `SHELLDLL_DefView` as a direct child.
    pub(crate) has_icons: bool,
}

/// Explorer's desktop windows at one moment.
#[derive(Clone, Debug)]
pub(crate) struct Snapshot<W> {
    pub(crate) progman: W,
    /// Progman has `WS_EX_NOREDIRECTIONBITMAP`, which is how Windows marks the
    /// raised desktop. It is set whether or not the `WorkerW` exists yet, so it
    /// is the one thing that tells "raised, not split yet" from "classic, not
    /// split yet".
    pub(crate) raised_style: bool,
    /// Progman's direct children, topmost first.
    pub(crate) progman_children: Vec<Seen<W>>,
    /// The top-level `WorkerW` windows, topmost first.
    pub(crate) top_workers: Vec<Seen<W>>,
}

/// Where the wallpaper windows go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout<W> {
    /// Classic: inside the empty `WorkerW` behind the icons.
    Behind { worker: W },
    /// Raised: children of Progman, below `icons` and above `layer`, Explorer's
    /// own wallpaper. Either can be missing: the icons when the user hid them,
    /// the layer until Explorer has made it.
    UnderIcons {
        progman: W,
        icons: Option<W>,
        layer: Option<W>,
    },
    /// Classic but not split. There is no room behind the icons, because the
    /// icon view draws the wallpaper itself; `0x052C` is what makes some.
    Unsplit { progman: W },
}

impl<W: Copy> Layout<W> {
    /// Whether asking Explorer to split the desktop could give a better answer.
    pub(crate) fn wants_split(&self) -> bool {
        matches!(self, Self::Unsplit { .. } | Self::UnderIcons { layer: None, .. })
    }

    /// The window our windows are made inside.
    pub(crate) fn parent(&self) -> W {
        match *self {
            Self::Behind { worker } => worker,
            Self::UnderIcons { progman, .. } | Self::Unsplit { progman } => progman,
        }
    }
}

pub(crate) fn choose<W: Copy + Eq>(snapshot: &Snapshot<W>) -> Layout<W> {
    let child = |class: &str| {
        snapshot
            .progman_children
            .iter()
            .find(|seen| seen.class == class)
            .map(|seen| seen.window)
    };
    let (icons, layer) = (child(ICONS), child(WORKER));

    // Both inside Progman, in either order, is the raised desktop even on the
    // builds that made it before they started setting the style bit.
    if snapshot.raised_style || (icons.is_some() && layer.is_some()) {
        return Layout::UnderIcons {
            progman: snapshot.progman,
            icons,
            layer,
        };
    }

    // Only a `WorkerW` holding the icons counts. Anything else with a
    // `SHELLDLL_DefView` inside, such as an old-style file dialog, is not the
    // desktop, and the `WorkerW` after one of those can be the icons' own.
    let holding = snapshot.top_workers.iter().position(|seen| seen.has_icons);
    if let Some(at) = holding
        && let Some(behind) = snapshot
            .top_workers
            .iter()
            .skip(at + 1)
            .find(|seen| !seen.has_icons)
    {
        return Layout::Behind {
            worker: behind.window,
        };
    }

    Layout::Unsplit {
        progman: snapshot.progman,
    }
}

/// What to move so that `ours` sit below the icons and above Explorer's layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Restack<W> {
    /// Ours that are anywhere else, including not among Progman's children at
    /// all. Each goes directly below the icons, or to the top without them.
    pub(crate) lift: Vec<W>,
    /// Explorer's layer is above the icons, where it hides them and anything
    /// below them, and goes to the bottom.
    pub(crate) sink_layer: bool,
}

impl<W> Restack<W> {
    pub(crate) fn is_needed(&self) -> bool {
        !self.lift.is_empty() || self.sink_layer
    }
}

/// Compare Progman's children, topmost first, with where ours should be.
///
/// Nothing is moved that is already in place: moving windows every poll is
/// what makes a desktop flicker.
pub(crate) fn restack<W: Copy + Eq>(
    children: &[W],
    ours: &[W],
    icons: Option<W>,
    layer: Option<W>,
) -> Restack<W> {
    let at = |window: W| children.iter().position(|child| *child == window);
    let icons_at = icons.and_then(at);
    let layer_at = layer.and_then(at);

    // A layer above the icons goes to the bottom, so being below it for now
    // does not count against ours.
    let sink_layer = matches!((layer_at, icons_at), (Some(layer), Some(icons)) if layer < icons);
    let lift = ours
        .iter()
        .copied()
        .filter(|window| match at(*window) {
            None => true,
            Some(mine) => {
                icons_at.is_some_and(|icons| mine < icons)
                    || (!sink_layer && layer_at.is_some_and(|layer| mine > layer))
            }
        })
        .collect();

    Restack { lift, sink_layer }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(window: u32, class: &str, has_icons: bool) -> Seen<u32> {
        Seen {
            window,
            class: class.to_owned(),
            has_icons,
        }
    }

    fn snapshot(
        raised_style: bool,
        progman_children: Vec<Seen<u32>>,
        top_workers: Vec<Seen<u32>>,
    ) -> Snapshot<u32> {
        Snapshot {
            progman: 1,
            raised_style,
            progman_children,
            top_workers,
        }
    }

    #[test]
    fn a_classic_split_desktop_puts_the_wallpaper_in_the_empty_worker() {
        let tree = snapshot(
            false,
            vec![],
            vec![seen(10, WORKER, true), seen(11, WORKER, false)],
        );
        assert_eq!(choose(&tree), Layout::Behind { worker: 11 });
    }

    #[test]
    fn a_classic_desktop_before_the_split_asks_for_one() {
        let tree = snapshot(false, vec![seen(20, ICONS, false)], vec![]);
        let layout = choose(&tree);
        assert_eq!(layout, Layout::Unsplit { progman: 1 });
        assert!(layout.wants_split());
    }

    #[test]
    fn only_the_worker_right_behind_the_icons_is_ours() {
        // Only `WorkerW` windows are looked at, so an old-style file dialog
        // with a DefView of its own never is; and a stray empty `WorkerW`
        // above the real pair is not the one behind the icons.
        let tree = snapshot(
            false,
            vec![],
            vec![
                seen(9, WORKER, false),
                seen(10, WORKER, true),
                seen(11, WORKER, false),
            ],
        );
        assert_eq!(choose(&tree), Layout::Behind { worker: 11 });
    }

    #[test]
    fn the_icons_own_worker_is_never_chosen() {
        let tree = snapshot(false, vec![], vec![seen(10, WORKER, true)]);
        assert_eq!(choose(&tree), Layout::Unsplit { progman: 1 });
    }

    #[test]
    fn a_raised_desktop_goes_under_the_icons_and_over_the_layer() {
        let tree = snapshot(
            true,
            vec![seen(20, ICONS, false), seen(21, WORKER, false)],
            vec![],
        );
        let layout = choose(&tree);
        assert_eq!(
            layout,
            Layout::UnderIcons {
                progman: 1,
                icons: Some(20),
                layer: Some(21)
            }
        );
        assert!(!layout.wants_split());
        assert_eq!(layout.parent(), 1);
    }

    #[test]
    fn a_raised_desktop_with_its_layer_on_top_is_still_raised() {
        let tree = snapshot(
            true,
            vec![seen(21, WORKER, false), seen(20, ICONS, false)],
            vec![],
        );
        assert_eq!(
            choose(&tree),
            Layout::UnderIcons {
                progman: 1,
                icons: Some(20),
                layer: Some(21)
            }
        );
    }

    #[test]
    fn a_raised_desktop_without_its_layer_yet_never_goes_over_the_icons() {
        let tree = snapshot(true, vec![seen(20, ICONS, false)], vec![]);
        let layout = choose(&tree);
        assert_eq!(
            layout,
            Layout::UnderIcons {
                progman: 1,
                icons: Some(20),
                layer: None
            }
        );
        assert!(layout.wants_split());
    }

    #[test]
    fn a_raised_desktop_with_the_icons_hidden_still_counts() {
        let tree = snapshot(true, vec![seen(21, WORKER, false)], vec![]);
        assert_eq!(
            choose(&tree),
            Layout::UnderIcons {
                progman: 1,
                icons: None,
                layer: Some(21)
            }
        );
    }

    #[test]
    fn both_inside_progman_is_raised_without_the_style_bit() {
        let tree = snapshot(
            false,
            vec![seen(20, ICONS, false), seen(21, WORKER, false)],
            vec![],
        );
        assert!(matches!(choose(&tree), Layout::UnderIcons { .. }));
    }

    #[test]
    fn the_style_bit_wins_over_a_stale_classic_pair() {
        let tree = snapshot(
            true,
            vec![seen(20, ICONS, false)],
            vec![seen(10, WORKER, true), seen(11, WORKER, false)],
        );
        assert!(matches!(choose(&tree), Layout::UnderIcons { .. }));
    }

    #[test]
    fn nothing_moves_when_everything_is_in_place() {
        let fix = restack(&[20, 30, 31, 21], &[30, 31], Some(20), Some(21));
        assert!(!fix.is_needed(), "{fix:?}");
    }

    #[test]
    fn a_new_window_above_the_icons_is_lifted_under_them() {
        let fix = restack(&[30, 20, 21], &[30], Some(20), Some(21));
        assert_eq!(fix.lift, vec![30]);
        assert!(!fix.sink_layer);
    }

    #[test]
    fn a_new_window_below_the_layer_is_lifted_above_it() {
        // Lifted to just below the icons is above a layer that is below them.
        let fix = restack(&[20, 21, 30], &[30], Some(20), Some(21));
        assert_eq!(fix.lift, vec![30]);
        assert!(!fix.sink_layer);
    }

    #[test]
    fn a_layer_explorer_made_again_on_top_is_sunk() {
        let fix = restack(&[21, 20, 30], &[30], Some(20), Some(21));
        assert!(fix.lift.is_empty(), "{fix:?}");
        assert!(fix.sink_layer);
    }

    #[test]
    fn a_layer_on_top_and_a_window_over_the_icons_both_move() {
        let fix = restack(&[21, 30, 20], &[30], Some(20), Some(21));
        assert_eq!(fix.lift, vec![30]);
        assert!(fix.sink_layer);
    }

    #[test]
    fn a_window_that_left_progman_is_put_back() {
        let fix = restack(&[20, 21], &[30], Some(20), Some(21));
        assert_eq!(fix.lift, vec![30]);
    }

    #[test]
    fn without_icons_ours_only_have_to_be_above_the_layer() {
        assert!(!restack(&[30, 21], &[30], None, Some(21)).is_needed());
        let fix = restack(&[21, 30], &[30], None, Some(21));
        assert_eq!(fix.lift, vec![30]);
        assert!(!fix.sink_layer);
    }

    #[test]
    fn without_a_layer_ours_only_have_to_be_below_the_icons() {
        assert!(!restack(&[20, 30], &[30], Some(20), None).is_needed());
        assert_eq!(restack(&[30, 20], &[30], Some(20), None).lift, vec![30]);
    }
}
