# sonora

A native music streaming client, built with Rust and [GPUI](https://github.com/zed-industries/zed)
(Zed's UI framework). Cargo workspace, edition 2024, resolver 3.

## Crate layout

```
crates/
  sonora/     binary: main, window, actions, asset registry, HTTP client shim, tray, dock
  views/      screens, shells (Workspace, FullscreenView) and app chrome
  state/      GPUI entities holding app state, and all async orchestration
  music/      provider traits and models in the root, one submodule per provider (no GPUI)
  storage/    the state.sqlite and cache.sqlite paths, schema and connection setup
  ui/         design system: theme, metrics, layout ladder and reusable elements
  router/     Destination enum, navigation history, Link trait
  input/      text input element, global actions and keybindings
  i18n/       Fluent localization: the `t!` macro, locale selection, embedded .ftl
  icons/      the icon packs: registry, active pack, path resolution, AssetSource
  embed/      build-script helper that walks a folder and writes include_bytes! literals
  webview/    a native browser window over a throwaway session, for cookie sign-ins and PO tokens
  widevine/   the Widevine CDM host, pssh and CENC fMP4 parsing, for any DRM'd provider
```

Dependency direction is strict. Do not create a back edge:

```
sonora → views → state → music
         state, music → storage
         state → webview
         music → widevine
         all ui-side crates → ui, router, input → ui → gpui
         every ui-side crate → i18n, icons → gpui
```

- `state` and `views` see only the traits and models in the root of `music`, never a provider
  module. Only `sonora/src/main.rs` names a concrete provider.
- `music` never depends on `gpui`. `ui` never knows about `music`, `state` or playback. Widgets
  that need app state live in `views/src/chrome/`.
- `music::engine` is the one playback engine and `music::stream` the one progressive download. A
  provider implements `engine::Fetch` and `stream::Body` rather than writing its own. The engine
  applies loudness normalisation, so a provider only reports a track's level through
  `Fetch::loudness`. Spotify is the one exception, because librespot decodes and normalises
  inside its own player.
- `music::drm` is the only way `state` and `views` reach protected playback. The CDM is never
  shipped in any artefact or package.

## Rules

- **Reuse before you build.** Check `crates/ui/src/lib.rs`, `crates/views/src/shared/` and
  `crates/views/src/chrome/` first, and extend an element with a builder method rather than
  writing a sibling. New elements copy the shape of `ui/src/button.rs`, including the
  `mem::take` and `refine` of caller styles.
- **Never call the Spotify Web API.** No `reqwest` to `api.spotify.com`, no client secret, no
  `rspotify`. Spotify data comes from librespot's `spclient`, and auth uses Spotify's own client id
  in `auth.rs`.
- **Never hardcode a color, radius or size.** Read them from `cx.theme()` and `theme.metrics`. A
  literal `px(…)` is allowed only for a local, non-scaling detail declared as a module `const`. A
  new color token goes into `Theme`, every `Theme::*()` constructor, `ThemeOverrides` and
  `apply_color!`.
- **Never write a bare breakpoint.** Use the `ui::Room` ladder, and classify width with
  `Chrome::room` or `Chrome::content`, never the raw viewport. A view whose layout depends on
  width observes `Chrome::entity(cx)`.
- **Every user-facing string comes from Fluent.** Add the key to `assets/i18n/en-US/main.ftl`. Other
  locales may lag. Never call `t!` in a constructor: store the key and resolve it in `render`.
  Counts use Fluent selectors. Log messages and `.context(…)` stay in English.
- **Every interactive element is announced by screen readers.** A node reaches the
  accessibility tree only with both `.id()` and `.role()`; pair every role with an
  `aria_label` resolved from Fluent in `render` (icon-only buttons take their name from the
  tooltip). Keep ids stable across frames and unique within a list: wrap rows in
  `div().id(("kind", stable_id))` with `aria_position_in_set`/`aria_size_of_set`, never a bare
  row index. Extend `crates/ui/tests/a11y_tree.rs` with every new primitive.
- **Network work runs on tokio.** Anything touching `MusicApi`, librespot or sockets goes inside
  `Io::global(cx).spawn`, awaited from `cx.spawn` and applied in `this.update` ending with
  `cx.notify()`. Store the returned `Task` in a field and never detach a data load. Network-backed
  features belong in a `state` entity, not a view. An entity that caches provider data clears on
  `SessionEvent::SignedOut`.
- **Never drive a player from a view.** Go through `state::Playback`.
- **Assets are picked up from their folder.** Drop an icon into `assets/icons/<pack>/` or a face
  into `assets/fonts/`. Call sites spell `"icons/<name>.svg"` and resolve it with `icons::path` at
  render.
- **Generated files are regenerated, never edited.** App icons come from
  `scripts/generate-icons.py`, `THIRD-PARTY.md` from `scripts/generate-notices.py`, the README
  translation table from `scripts/i18n-coverage.py`.
- **Do not add tests unless asked.** Offering is fine. Existing tests are `#[cfg(test)] mod tests`
  at the bottom of the file they cover. Accessibility regression tests in
  `crates/ui/tests/a11y_tree.rs` are the exception: extend them with every new primitive.

## Building and checks

```sh
nix develop                    # or direnv allow
cargo run --locked --package sonora
cargo fmt
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

The devShell has no `cargo` or `cargo-clippy`; those come from the system profile. The flake's
default package is the released binary, so `nix build` never sees the working tree, while
`nix build .#sonora-src` compiles it.
`.cargo/config.toml` links with mold on `x86_64-unknown-linux-gnu`, so mold must be on PATH, or
build with `RUSTFLAGS=""`. `crates/widevine` compiles C++, so a C++ compiler is needed too.

Clippy is clean, `--all-targets` included, and stays that way. Boxed callback fields get a
module-local `type` alias rather than a `type_complexity` warning. The one `#[allow]` is
`reversed_empty_ranges` on the `clamp_range("abc", &(2..1))` test in `crates/ui/src/input/mod.rs`.

## Code style

- Every type and every function whose behaviour is not obvious from its name carries a `///`
  comment saying what it is for and what a caller has to know. One or two plain sentences.
- `use gpui::prelude::*;` then explicit imports. Traits are imported anonymously
  (`use ui::ActiveTheme as _;`).
- Module order: `use` block, `const`s, types, impls, private free helpers at the bottom.
- In render, prefer `.when()`, `.when_some()` and `.map()` over branching. Two-arm boolean choices
  use `match flag { true => …, false => … }`. Use let-else and return early.
- `anyhow::Result` at boundaries with lowercase `.context("cannot …")`. Logs are prefixed by
  subsystem (`"playback: …"`).
- Dependencies go in the root `[workspace.dependencies]`. `gpui` and `gpui_platform` are pinned to
  one git rev and move together.

## Commits

Conventional Commits: `type(scope): description`, imperative, lowercase, no trailing period, no
body. Scopes in use: `views`, `ui`, `music`, `state`, `playback`, `player`, `settings`, `router`,
`local`, `sonora`, `nix`.

## Pull requests

One heading, one list, nothing else:

```markdown
## Summary

- add playlist create, rename, delete, visibility, and track mutation support
- add album and playlist queue actions
```

Bullets are lowercase, imperative, one line each, and describe behaviour rather than files.

## Changelog

`CHANGELOG.md` follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Add to
`## [Unreleased]` as features land, under `Added`, `Changed` or `Fixed`. Each entry is one short
line saying what someone using Sonora can now do, since the notes are posted to Discord. Leave out
implementation details and any work no user can observe. Cutting a release is the
`release-sonora` skill.

## This copy (`jasonb589/sonora`)

This checkout is a fork maintained without a Rust toolchain: nothing here is built or tested
locally, and every check runs on GitHub Actions.

- **Code changes land through a PR with a green `ci` run** (fmt, clippy `-D warnings`,
  `cargo test --workspace --locked`, all on `windows-latest`). Write code in rustfmt's style by
  hand: `max_width` 100, comments are not reflowed, and a let-chain breaks before `&&`. A
  docs-only change may be pushed straight to `main`, which runs no workflow.
- **Releases.** `gh workflow run windows.yml -R jasonb589/sonora --ref main -f version=X`. The
  tag is `windows-X` (upstream's `v*` tags are never fetched, so they cannot collide), the title
  is that same version, and the notes put a hand-written "What's new" above the workflow's
  auto-generated "What's Changed". The installer is handed out from the release alone.
- **Translations.** `zh-CN` is kept at every key `en-US` has, and new keys are translated in the
  same change. `scripts/i18n-coverage.py` needs Python this machine does not have, so the
  README's table is regenerated with the PowerShell port of it.
- **Upstream syncs use a merge commit, never a squash.** Squashing throws upstream's parents
  away, so the next sync replays every upstream commit since the last *shared* commit instead of
  the delta — the 0.42.2 sync paid for 0.42.1 a second time for exactly this reason. Merge
  `upstream/main` into `sync/upstream-X.Y.Z` and merge that branch into `main` with `--merge`.
  0.43.0 was the first sync done that way, so `upstream/main` is an ancestor of `main` from then
  on and the next base is the release that was merged.
- **A sync conflict is decided by which side changed the file since the last synced upstream
  commit:** `git diff <that commit> main -- <file>` against
  `git diff <that commit> upstream/main -- <file>`. One side changed it → take that side. Both
  changed it → merge by hand. `README.md`'s translation table is regenerated rather than merged.
- **A merge that reports no conflict can still be wrong.** A file only one side had touched came
  out with both sides' content, which showed up as a duplicated function in
  `crates/sonora/src/logging.rs`. For every such file the merged tree must equal that side:
  `git diff --quiet <side> HEAD -- <file>` per file, and in PowerShell that answer is in
  `$LASTEXITCODE`, not in the output a `-not (...)` would read. Afterwards check that the dead
  workflows this copy removed did not come back, that `Cargo.toml` and `Cargo.lock` are
  consistent, and that `zh-CN` covers the new keys.
- **`crates/state/src/settings.rs` carries one fork-only line:** its state-store test drops the
  store before removing the database, because the kept connection holds the file open and Windows
  refuses to delete it. Keep that line when the file conflicts.
- **Upstream renames break the fork's own provider code silently.** 0.44.0 turned
  `Shape::Catalog` into `Shape::Catalog(Catalog::Fixed)`; only clippy found it, because the
  fork-only `music/src/navidrome/` files are the only ones that name the variant. Compile the
  workspace through CI before assuming a conflict-free merge is a buildable one.
- **`crates/i18n` forbids a locale from carrying a key `en-US` lacks** (`no_locale_invents_a_key`),
  so a sync that drops en-US keys needs them pruned out of `zh-CN` too. A dropped key the code
  still asks for, like the shelf titles `home-newest-albums` and `home-top-songs`, has to go back
  into `en-US` instead. `crates/views/src/screens/settings/mod.rs` now holds three exhaustive
  `match slot` blocks, one of which is the label table, and a new `Slot` needs an arm in each.
- **Clippy denies `items_after_test_module`,** so a block appended to the end of a file that ends
  in `#[cfg(test)] mod tests` has to go above it. `crates/state/src/settings.rs` keeps the fork's
  `immersive` getter and setter in their own `impl AppSettings` block ahead of the tests rather
  than inside the file's big impl.
- **Upstream bumps `Cargo.toml` and `Cargo.lock` on every release,** so a sync brings their
  version in and the app's About row starts reporting the release it was built from. The fork's
  own numbering has to step past that: 0.44.0 was synced as 0.44.0 and its Chinese pass shipped as
  0.44.1, so upstream's 0.44.1 shipped here as **0.44.2**, with `Cargo.toml`'s workspace version
  and the thirteen workspace entries in `Cargo.lock` bumped in the same commit.
- **A new `SessionEvent` variant lands in state's `match` arms,** one per owner; the fork's own
  arm in `crates/state/src/cover.rs` is the only one git cannot merge on its own, since it is the
  only arm the fork changed.
