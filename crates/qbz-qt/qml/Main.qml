// Root window: dark background + a Loader keyed on the bridge's `screen`
// property ("splash" | "login" | "shell"). Kicks off the Rust boot
// sequence once the QML tree is complete (the bridge registers its Qt
// thread handle in that first invokable, so every async UI hop lands).
//
// Font: main.rs applies an explicitly selected bundled family before this
// document is built. With "System" it applies nothing, so both plain Text and
// Qt Quick Controls inherit the operating-system font chosen by Qt.

import QtQuick
import QtQuick.Controls
import QtQuick.Window
import com.blitzfc.qbz
import "controls"
import "miniplayer"
import "theme"

ApplicationWindow {
    id: window
    // Geometry is RESTORED, not hardcoded (crates/qbz/src/main.rs:8211-8282 is
    // the Slint restore; :1399-1435 is its save). The bridge seeds these at
    // CONSTRUCTION from the shared ui_prefs.json, so the very first frame is
    // already the user's size — a post-boot push would arrive after layout.
    // `windowWidth`/`windowHeight` already carry the never-saved fallback
    // (1180x760: app.slint's preferred size), so there is no local default to
    // disagree with Slint about.
    //
    // This was the title-truncation bug: at the old hardcoded 1280 the
    // now-playing bar sat one responsive tier below the Slint build on the same
    // machine (1280 < 1366 -> side fraction 0.39 instead of 0.30), so the
    // Classic song card received ~446px instead of the larger mid-tier runway
    // and elided titles early. The tier thresholds were right; the window was
    // the wrong size. The card now also measures its live neighbouring controls,
    // so restored geometry and in-bar allocation solve different parts of the
    // same problem rather than relying on a fixed card cap.
    //
    // These stay plain bindings so the size is correct before the window maps.
    // The WM's own resizes write `width`/`height` from C++ — that does not
    // disturb the binding, and nothing ever re-sets the bridge properties, so
    // the value never snaps back under the user.
    //
    // Slint's best-effort monitor clamp (main.rs:8242-8249) is folded INTO the
    // binding rather than applied afterwards: a size saved on the big display
    // must not strand the window on a smaller one, and the clamped number has
    // to be the MAPPED number. Done imperatively in Component.onCompleted (as
    // it was) it ran after componentComplete had already shown the window, so
    // the window flashed at the oversized geometry and then snapped — and the
    // JS assignment broke the binding and fired the save debounce, which is how
    // a 2560px-wide session got permanently rewritten to 1920 after one launch
    // on the laptop panel (for the Slint build too — same file).
    // SHRINK ONLY, and only while the screen itself clears the app floor:
    // Slint skips the clamp on exactly that condition instead of fighting the
    // WM over an impossible minimum. A screen we cannot resolve degrades to
    // "no clamp", like Slint's monitor query returning None.
    width: window.bootScreenWidth >= window.minimumWidth
           ? Math.min(QbzShell.windowWidth, window.bootScreenWidth)
           : QbzShell.windowWidth
    height: window.bootScreenHeight >= window.minimumHeight
            ? Math.min(QbzShell.windowHeight, window.bootScreenHeight)
            : QbzShell.windowHeight
    // Screen metrics for that clamp, read from `window.screen` (the window's
    // OWN screen; the attached `Screen` wants an Item). Frozen in
    // Component.onCompleted — see the freeze there for why they must stop
    // tracking.
    property real bootScreenWidth: window.screen ? window.screen.width : 0
    property real bootScreenHeight: window.screen ? window.screen.height : 0
    // app.slint:52-53 (`min-width: 940px / UiScale.factor`), carried through the
    // bridge. The old 800x600 let the window go below Slint's floor, which is
    // exactly where the responsive tiers stop being comparable.
    minimumWidth: QbzShell.kioskProfile ? 800 : QbzShell.windowMinWidth
    minimumHeight: QbzShell.kioskProfile ? 480 : QbzShell.windowMinHeight
    visible: true
    // Last session's maximized state, applied DECLARATIVELY so it is part of
    // the first mapped frame: QQuickWindowQmlImpl defers the show until
    // componentComplete and then honours whatever `visibility` holds, whereas
    // the old Component.onCompleted assignment landed AFTER the map and the
    // window visibly jumped from the floating size to maximized. Slint restores
    // before `window.run()` and never flashes (main.rs:8273-8281).
    // One-shot by construction: `windowMaximized` is seeded at bridge
    // construction and never rewritten, so this never re-evaluates, and
    // HeaderBar's imperative `hostWindow.visibility = ...`
    // (shell/HeaderBar.qml:76) removes the binding outright the first time the
    // user toggles — which is the wanted behaviour, not a leak.
    // Windowed (not AutomaticVisibility) on the false branch: a fresh window
    // must land in its natural state, matching "only ever re-apply a true".
    // The kiosk appliance arm comes FIRST and is one-shot by construction:
    // `kioskFullscreenBoot` is seeded at bridge construction from
    // `QBZ_PROFILE=kiosk` AND `QBZ_KIOSK_FULLSCREEN`, and nothing ever writes
    // it, so this can never re-evaluate and trap a desktop session
    // (incident 2026-07-11: the kiosk shell has no titlebar control and
    // neither Esc nor F11 leave fullscreen). Declarative for the same reason
    // the maximized arm is: QQuickWindowQmlImpl defers the show until
    // componentComplete, so this is part of the first mapped frame instead of
    // a visible jump afterwards.
    visibility: QbzShell.kioskFullscreenBoot
                ? Window.FullScreen
                : (QbzShell.windowMaximized ? Window.Maximized : Window.Windowed)
    // "Show track in window title" (Appearance). 1:1 with app.slint:44 —
    // FIXED format, no template, and it falls back to the plain app name
    // whenever the setting is off or nothing is loaded. Reactive: the
    // binding re-evaluates on every track change and on the toggle itself
    // (settings_qt pushes `windowTitleShow` live).
    title: QbzShell.isMacos && !QbzShell.systemTitleBar ? ""
        : QbzShell.windowTitleShow && QbzPlayer.npHasTrack
        ? QbzPlayer.npTitle + " - " + QbzPlayer.npArtist + " | qbz"
        : "QBZ"
    // Custom chrome (phase 7/12): frameless but OPAQUE — the phase-7
    // translucent window was a misread: the Slint MAIN window keeps an
    // OPAQUE swapchain (only the miniplayer blends; crates/qbz/src/main.rs
    // set_surface_prefers_transparent + the Cargo.toml patch comment), and
    // the rounded corners in the Slint screenshots come from the
    // COMPOSITOR, not the app — app.slint's root is opaque surface-main
    // with square corners and a square 1px hairline frame. The system
    // titlebar is the `use_system_title_bar` pref (ui_prefs.json; applied
    // at startup, restart semantics like Slint).
    // Three different windows, one per platform/mode:
    //
    //   system title bar   → a plain Qt.Window, the OS decorates it.
    //   macOS custom       → Qt.ExpandedClientAreaHint | Qt.NoTitleBarBackgroundHint.
    //   Linux/Windows      → frameless, and we draw the cluster ourselves.
    //
    // The macOS arm is FIRST-CLASS QT API (6.9+, and both machines run
    // 6.11.1), not a hand-rolled AppKit poke: `ExpandedClientAreaHint`
    // expands the client area into the region the titlebar controls occupy,
    // `NoTitleBarBackgroundHint` drops the titlebar's background — and the
    // native traffic lights STAY. Content then reaches y=0 and the QBZ header
    // sits under them.
    //
    // The previous attempt set `titlebarAppearsTransparent` +
    // `titleVisibility` + `NSWindowStyleMaskFullSizeContentView` on the
    // NSWindow from Rust, and could not win: `QCocoaWindow::windowStyleMask()`
    // RECOMPUTES the mask from Qt's own flags and reassigns it on
    // `setWindowFlags()`, on fullscreen enter/exit, and via `setWindowState()`
    // — preserving only the fullscreen and unified-toolbar bits, never
    // FullSizeContentView. The `visibility` binding below is itself a
    // `setWindowState()`, so the bit was wiped moments after being set. That
    // is why the log kept saying "traffic lights centred" while the owner kept
    // seeing a stock title bar. (Qt forum "Unable to get transparent title bar
    // in macOS since Qt 6.4", QTBUG-134797.)
    //
    // Frameless is NOT an option on macOS: it removes the traffic lights with
    // the frame, and the reference never draws its own there
    // ("Linux only — macOS keeps the native traffic lights",
    // WindowControls.slint:1-2).
    // WINDOWS takes the macOS shape, not the Linux one, and the difference is
    // the whole feature (W27).
    //
    // `Qt.FramelessWindowHint` removes WS_THICKFRAME, and THAT is what the
    // shell keys tiling on: Win+Arrow and Aero Snap by dragging to an edge both
    // stop, along with the DWM drop shadow and the Win11 rounded corners. So
    // "hide title bar" was not making the app refuse the shortcuts -- it had
    // stopped being a window the shell could tile.
    //
    // NOT restored, and it cannot be from flags alone: the Windows 11 Snap
    // Layouts flyout that appears when hovering a maximise button. Windows asks
    // for it by hit-testing that pixel as HTMAXBUTTON, and QBZ's maximise
    // button is a QML item inside the client area, so it answers HTCLIENT.
    // Wiring it would mean reporting HTMAXBUTTON over that one rect from the
    // native filter -- deliberate future work, not something this arm does.
    //
    // Qt 6.9's Windows plugin already implements WM_NCCALCSIZE -> 0,
    // DwmExtendFrameIntoClientArea, the eight resize edges and
    // safeAreaMargins() for `ExpandedClientAreaHint` -- the same flags the
    // macOS arm uses. `CustomizeWindowHint` with no button hints then
    // suppresses Qt's own painted title cluster
    // (qwindowswindow.cpp :3561/:3600/:3624/:3645) while the window KEEPS
    // WS_CAPTION, so the shell still treats it as tileable.
    //
    // It also provides no HTCAPTION drag band of its own (:3332-3338: with
    // both isDefaultTitleBar and isCustomized false the whole title block is
    // skipped), which is why HeaderBar's QML `startSystemMove()` stays: it
    // issues SC_MOVE, and SC_MOVE drags DO snap. The eight QML resize grips
    // stay enabled too and do not fight the native edges -- Qt answers
    // WM_NCHITTEST first, so a press on the border never reaches QML.
    flags: QbzShell.systemTitleBar
        ? Qt.Window
        : (QbzShell.isMacos
            ? (Qt.Window | Qt.ExpandedClientAreaHint | Qt.NoTitleBarBackgroundHint)
            : (QbzShell.isWindows
                // Both halves were MEASURED on the box, and neither is
                // optional:
                //
                // WITHOUT CustomizeWindowHint, Qt populates the default title
                // hints and its customized-title path DRAWS minimise/maximise/
                // close over the QBZ header.
                //
                // WITH it, Qt answers WM_NCHITTEST with HTNOWHERE for every
                // pixel -- centre, header, player bar and sidebar all returned
                // 0 -- and Windows sends no mouse input to HTNOWHERE, so the
                // whole app becomes unclickable while still running.
                //
                // No public flag combination gives no-buttons + clicks +
                // the WS_THICKFRAME that tiling needs, so the hint stays and
                // `win_shell::install_hittest_filter()` answers the hit test
                // instead. What this arm must NOT do is drop the hint: that
                // trades the buttons back in.
                //
                // Frameless is not the answer either: it removes
                // WS_THICKFRAME, and THAT is what Win+Arrow and Aero Snap
                // actually key on -- not WS_CAPTION, which this window does
                // not have in either arrangement.
                ? (Qt.Window | Qt.ExpandedClientAreaHint | Qt.NoTitleBarBackgroundHint
                   | Qt.CustomizeWindowHint)
                : (Qt.Window | Qt.FramelessWindowHint)))

    // THE OTHER HALF OF ExpandedClientAreaHint, and the piece that made the
    // flag look broken when it was not.
    //
    // Since 6.9 "ApplicationWindow will automatically add padding to the
    // contentItem for any safe area margins reported by the window", so that
    // "the contentItem stays inside the safe area of the window, while the
    // background item covers the entire window" (Qt Quick Controls docs).
    //
    // On macOS with the expanded client area that safe area IS the titlebar
    // band. So the window background dutifully covered the whole frame while
    // `screenLoader` — anchored to the contentItem — was pushed ~28pt down.
    // The result reads on screen as a stock title bar with the QBZ header
    // below it, which is precisely what the owner kept reporting while the
    // AppKit side measured perfect (styleMask 0x800f, titlebar transparent,
    // contentH == windowH == 1328).
    //
    // QBZ draws its own chrome edge to edge on every platform — the header at
    // the top, the player bar at the bottom, the sidebar at the left — so it
    // takes the safe area over from Qt entirely. `HeaderBar.chromeLeftInset`
    // is where that responsibility is discharged: it reads
    // `SafeArea.margins.left` to clear the traffic lights.
    // The four Window paddings are Qt 6.9 API (SafeArea landed in 6.9). The
    // Linux pin is Qt 6.8 LTS, where declaring them here refuses the whole
    // component ("Cannot assign to non-existent property") — measured on the
    // CI VM, 2026-08-25. They matter on BOTH ExpandedClientAreaHint arms:
    // macOS AND Windows (the Windows arm arrived later, in 415cba57d, and
    // this gate was not widened with it — the result was the caption-height
    // safe-area band auto-padding the contentItem down, painted with the
    // window's #1a1a1a background: a dark grey strip above the QBZ header,
    // owner-reported 2026-09-01). Set imperatively from a Loader that never
    // instantiates on Linux/6.8.
    Loader {
        active: QbzShell.isMacos || QbzShell.isWindows
        sourceComponent: Component {
            Item {
                Component.onCompleted: {
                    window.topPadding = 0
                    window.bottomPadding = 0
                    window.leftPadding = 0
                    window.rightPadding = 0
                }
            }
        }
    }
    color: "#1a1a1a"

    // Phase 23: every domain singleton boots (registers its Qt-thread
    // hop; QbzSession.boot additionally fires the app boot sequence).
    // ── The renderer probe ────────────────────────────────────────────────
    // The ONLY place that answers "what is actually drawing". QRhi decides it
    // when the scene graph initialises, so `main::apply_renderer_preference()`
    // — which runs before QGuiApplication — knows only what we ASKED for; a
    // driver that refuses the request lands us somewhere else entirely.
    //
    // The enum is mapped to a NAME here, so Rust never depends on
    // GraphicsInfo's numeric values (a Qt implementation detail). Rust ignores
    // "unknown", the value reported before the graph resolves, so the report
    // fires again for free once it does.
    //
    // This is the feature gate (shader scenes / dynamic background /
    // reduce-motion), NOT the per-item drawing check: items that need to pick
    // a draw path keep reading their own `GraphicsInfo.api`, which is per
    // window and needs no round trip. See src/renderer_qt.rs.
    Item {
        id: rendererProbe
        width: 0
        height: 0
        readonly property int api: GraphicsInfo.api
        function report() {
            var name = "unknown"
            switch (rendererProbe.api) {
            case GraphicsInfo.Software:     name = "software"; break
            case GraphicsInfo.OpenGL:       name = "opengl";   break
            case GraphicsInfo.Direct3D11:   name = "d3d11";    break
            case GraphicsInfo.Direct3D12:   name = "d3d12";    break
            case GraphicsInfo.Vulkan:       name = "vulkan";   break
            case GraphicsInfo.Metal:        name = "metal";    break
            case GraphicsInfo.Null:         name = "null";     break
            }
            QbzShell.reportRendererApi(name)
        }
        onApiChanged: report()
        Component.onCompleted: report()
    }

    // ── The frame-liveness watchdog ───────────────────────────────────────
    // Disarms the startup sentinel that `apply_renderer_preference()` armed
    // for a forced renderer. The proof it waits for is FRAMES, not a window: a
    // backend can create a window and then never present, which from the
    // outside is indistinguishable from a healthy start until the user is
    // looking at a frozen pane — and that is exactly the state the sentinel
    // must NOT accept as success.
    //
    // 10 s rather than the first swap: a renderer that dies two seconds in
    // would otherwise have already been declared healthy, and the next launch
    // would keep re-forcing it. The reference waits for real user input with a
    // 30 s no-touch fallback (`disarm_renderer_sentinel_on_liveness`); frames
    // are the same idea with a signal Qt gives us directly.
    property int _framesSwapped: 0
    onFrameSwapped: window._framesSwapped++
    Timer {
        interval: 10000
        repeat: false
        running: true
        onTriggered: QbzShell.reportFrameLiveness(window._framesSwapped)
    }

    Component.onCompleted: {
        // DIAGNOSTIC (macOS chrome): the Rust side reports what AppKit ended
        // up with; this reports what QML asked for. Together they pin the
        // failure to either "the request was wrong" or "Qt ignored it".
        if (QbzShell.isMacos) {
            console.log("[macos-chrome] QML flags=" + window.flags
                + " systemTitleBar=" + QbzShell.systemTitleBar
                + " expected=" + (Qt.Window | Qt.ExpandedClientAreaHint
                                  | Qt.NoTitleBarBackgroundHint)
                // What Qt reports as unsafe, so we know whether
                // HeaderBar.chromeLeftInset is using Qt's own number or
                // falling back to the reference's 78px floor.
                + " safeArea(t/l/r/b)=" + window.SafeArea.margins.top
                + "/" + window.SafeArea.margins.left
                + "/" + window.SafeArea.margins.right
                + "/" + window.SafeArea.margins.bottom)
        }
        QbzSession.boot()
        // Qobuz Connect (2026-08-01 contract §2/§8). Booted right after
        // QbzSession because QbzSession.boot fires the whole app boot
        // sequence, whose shell entry runs the §11.5 service wiring — the
        // badge/devices publishes it makes must find the Qt-thread hop
        // already registered or they are dropped silently.
        QbzQConnect.boot()
        // Hotkeys layer (2026-08-03 hotkeys-port contract, block B1): boot
        // after QbzSession like every domain singleton. The B2 dispatcher
        // (AppShell-root Keys.onPressed) is the only other QML side.
        QbzHotkeys.boot()
        QbzLink.boot()
        // Search (2026-08-03 cortinilla-parity contract, commit C0): the
        // domain extracted from QbzBridge. Its position among the other
        // domain singletons is not load-bearing — every boot() here runs
        // before the first key or click — but the LINE is: boot() is what
        // registers the Qt-thread hop, and without it `search_bridge::ui()`
        // is a silent no-op, so the dropdown would never repaint and
        // nothing would be logged (TRACK-RULES, singleton boot order).
        QbzSearch.boot()
        QbzShell.boot()
        QbzPlayer.boot()
        QbzQueue.boot()
        QbzHome.boot()
        QbzViz.boot()
        // Immersive mode (2026-08-02 immersive-port contract, block B1).
        // Booted right after QbzViz: the overlay's open funnel drives the
        // viz two-source enable (§4.2), and ambient_qt publishes glow_color/
        // atmosphere_url to this singleton on every track change — a missing
        // boot line would drop those publishes silently (see above).
        QbzImmersive.boot()
        // Shader scenes (2026-08-15 immersive-completion contract, block A1).
        // Booted right after QbzImmersive: its open funnel resets the scene
        // through this singleton's hop, and the viz drain publishes the audio
        // pack here on every tick while immersive is open — a missing boot
        // line makes both silent no-ops (TRACK-RULES, singleton boot order).
        QbzShaderScene.boot()
        // Immersive Suggestions (the same contract, block B4 §4.5) — booted
        // WITH its bridge: every publish the suggestions loader makes rides
        // this hop, so a missing line is the forever-"{}" silent no-op the
        // comment below warns about (trap 2).
        QbzSuggestions.boot()
        QbzLocal.boot()
        QbzLibrary.boot()
        QbzAlbum.boot()
        QbzArtist.boot()
        QbzScene.boot()
        QbzMusician.boot()
        QbzLyrics.boot()
        QbzCast.boot()
        QbzOrbit.boot()
        QbzBridge.boot()
        // MyQBZ (two grids + detail + modals), the app-wide Add picker, the
        // Artist-Collection builder, and the Blacklist manager.
        //
        // boot() is what registers each singleton's CxxQtThread hop, so a
        // missing line here is a SILENT no-op forever: the view mounts, every
        // `ui()` publish from Rust is dropped on the floor, the document stays
        // at its "{}" default and NOTHING is logged on either side. QbzBlacklist
        // is the loudest case — its `blacklistLoading` defaults to true, so the
        // manager would spin forever.
        QbzMyQbz.boot()
        QbzMyQbzAdd.boot()
        QbzDisco.boot()
        QbzBlacklist.boot()
        QbzPlaylistPicker.boot()
        // The folder modals. Its boot() also publishes both closed documents,
        // which is what puts the icon-preset and colour-swatch constants on
        // the QML side BEFORE the first open rather than one turn after it.
        QbzFolderEdit.boot()
        // The SHARED playlist editor (rename · description · offline-only ·
        // delete), opened from the manager's three delegates, the sidebar row
        // menu and the playlist detail header's pencil. Its boot() publishes
        // the closed document so the modal's parse sees the full shape.
        QbzPlaylistEdit.boot()
        // Playlist Manager. Its boot() also seeds the default document and
        // kicks the cache-independent folder read, so the SIDEBAR's folder
        // consumers have a list before the manager view has ever been opened.
        QbzPlaylistManager.boot()
        // Playlist Importer. Nothing to seed — the modal is closed until a
        // sidebar row opens it — but WITHOUT this line every publish the
        // importer makes (the whole progress panel, the log, the summary) is
        // dropped on the floor with nothing logged on either side.
        QbzPlaylistImport.boot()
        // "Find available version" (2026-08-17 unavailable-tracks contract §6).
        // Nothing to seed — the modal is closed until a playlist row's context
        // menu opens it — but without this line every publish the controller
        // makes is dropped on the floor: the modal would open empty and keep
        // spinning, since its `loading` flag is only ever taken down by a
        // publish.
        QbzTrackReplace.boot()
        // The disc-metadata modal. Same story: closed until the pane's search
        // button opens it, and without this line the search would spin
        // forever, because only a publish takes `searching` back down.
        QbzDiscMeta.boot()
        // Local album metadata editor. Its controller publishes from worker
        // threads and the app-wide modal survives the album republish on save.
        QbzTagEditor.boot()
        // HiFi Wizard. Nothing to seed — the modal is closed until the
        // Settings > Audio row opens it — but without this line every publish
        // the wizard makes is dropped on the floor: the health verdict, the
        // enumerated DACs, the generated config and the live read-back would
        // all stay at their defaults, and nothing would be logged on either
        // side. The wizard is the loudest case after QbzBlacklist, because its
        // first step ends on "Checking your audio stack…" forever.
        // Linux only: the wizard's row is hidden elsewhere (AudioSettings.qml),
        // so booting it would spin up an audio-stack probe nothing can reach.
        if (QbzShell.isLinux)
            QbzDacWizard.boot()
        // Offline Cache Manager. Nothing to seed — the view is unmounted until
        // the Settings row opens it — but without this line the manager's very
        // first publish is dropped and the view sits on its spinner forever:
        // `managerLoading` DEFAULTS to true precisely so the empty state does
        // not flash, which turns a missing boot() into a permanent spinner
        // with nothing logged on either side.
        QbzOffline.boot()
        // Miniplayer (2026-08-03 miniplayer/tray contract, block B1). Booted
        // like every other domain singleton, after QbzSession: `boot()` is what
        // registers the Qt-thread hop, and without it `mini_bridge::ui()` is a
        // SILENT no-op — the mini queue document would never reach QML and
        // nothing would be logged on either side.
        QbzMini.boot()
        // Same reason, and it bites harder here: the tray's clicks reach QML
        // ONLY through this bridge's signals, so an unbooted QbzTray is a tray
        // whose every click does nothing, with zero evidence in the log
        // (contract §15 trap 27). The Connections that answer those four
        // signals are below, beside the window verbs they call.
        QbzTray.boot()
        // About / What's New. Its four invokables self-register the hop, so
        // today's menu-driven paths work without this line — but the FIRST
        // publish that is not preceded by an invokable (an avatar prefetch at
        // startup, an auto-show, a "new version" badge) would be a silent
        // no-op. Booting it here keeps it in the same contract as the other 34.
        QbzAbout.boot()
        // Purchases. Unlike QbzAbout above, this one has NO self-registering
        // invokable to fall back on: `purchases_bridge::ui()` is a silent no-op
        // until the Qt-thread hop exists, so without this line every publish is
        // dropped and both purchase screens stay permanently empty with nothing
        // logged. Position is free (only QbzSession and QbzQConnect are ordered).
        QbzPurchases.boot()

        // Seed the maximized latch ONCE, imperatively — see its declaration for
        // why it must not be a binding on the bridge property.
        window.maximizedLatch = QbzShell.windowMaximized

        // Freeze the clamp latch. The width/height bindings above read these,
        // and `window.screen` CHANGES when the user drags the window to another
        // monitor — a live dependency would re-run those bindings and snap the
        // window back to the restored size under the user's hands (the bindings
        // survive WM resizes, which come from C++ and do not clear them).
        // Assigning a property from imperative JS removes its binding, so
        // after these two lines the latch is a dead number and the geometry
        // bindings can never re-evaluate. Via locals, not self-assignment, so
        // it cannot be mistaken for a no-op.
        var latchedW = window.bootScreenWidth
        var latchedH = window.bootScreenHeight
        window.bootScreenWidth = latchedW
        window.bootScreenHeight = latchedH
    }

    // --- macOS chrome ----------------------------------------------------
    // The overlay attributes + centring the native traffic lights against the
    // 42px header. NOT in Component.onCompleted: it reaches the NSWindow
    // through `-[NSApplication mainWindow]`, which is nil until AppKit has
    // actually shown and keyed the window, and completion runs before that.
    // The first rendered frame is the earliest point where it is guaranteed.
    //
    // RETRIED UNTIL IT REPORTS SUCCESS. The first version latched "applied"
    // before calling, on `afterRendering`. That looked safe — the Rust side is
    // idempotent, so latching only avoided per-frame work — but AppKit has no
    // `mainWindow` yet at the first rendered frame, so the one attempt failed,
    // the latch stuck, and the chrome never applied. On the owner's Mac the
    // whole thing degraded to "stock title bar, plus a 78px inset reserved for
    // traffic lights that were in another bar", and the log said it exactly
    // once: `[macos-chrome] no main window yet`.
    //
    // A Timer rather than `afterRendering`: this must keep trying for a
    // moment, and the frame signal would run the AppKit maths at the display
    // rate to do it (the "changes less often than the frame rate" mistake this
    // port already paid for in VizSettle). ~5 s of 200 ms attempts is far more
    // than a window needs to become key, and it stops on the first success.
    Timer {
        id: macChromeTimer
        interval: 200
        repeat: true
        running: QbzShell.isMacos
        property int attempts: 0
        onTriggered: {
            attempts++
            if (QbzShell.applyMacChrome()) {
                running = false
                // NO geometry nudge here. An earlier version did a 1px
                // height round trip to force Qt to re-derive its content
                // rect — obsolete now that the expanded client area comes
                // from Qt's own flags, and actively wrong: `height` is a
                // BINDING (the restore clamp above), and assigning it from
                // imperative JS destroys that binding. This file warns about
                // exactly that trap a few lines further down, about the
                // maximized latch.
            } else if (attempts >= 25) {
                running = false
                console.warn("[macos-chrome] gave up after " + attempts + " attempts")
            }
        }
    }

    // --- Geometry persistence -------------------------------------------
    // Slint saves from the winit Resized/Moved handlers (main.rs:1399-1454) and
    // relies on a change guard to survive the no-op events a WM emits. QML has
    // no equivalent of "the WM settled", so the debounce stands in for it: a
    // drag fires widthChanged on every frame and only the last one is worth a
    // file write. The floating-only rule, the app minimum and the >0.5px dirty
    // check all live Rust-side in settings_qt::save_window_geometry, so every
    // call site here can stay a single unconditional line.
    //
    // What CANNOT be left to Rust is the maximized flag itself. Qt's
    // `visibility` is ONE enum in which Minimized REPLACES Maximized, while
    // winit — what Slint reads through `is_maximized()` (main.rs:1404-1408) —
    // keeps the two orthogonal and still reports maximized while iconified.
    // Deriving the flag from the live visibility therefore persisted
    // `window_maximized: false` on every minimize, and since the restore only
    // ever re-applies a true (main.rs:8273-8281) BOTH frontends then opened
    // un-maximized. So the flag is LATCHED from the last non-transient
    // visibility, and the transient states persist nothing at all.
    // Seeded imperatively in Component.onCompleted, NOT bound to
    // QbzShell.windowMaximized. cxx-qt generates a NOTIFY for every
    // #[qproperty], so a binding here would be a live reactive edge into the
    // latch: the day anything pushes window_maximized back to QML (syncing the
    // header icon after a WM-side maximize is the obvious future reason), the
    // binding re-fires on any session where the user has not yet toggled
    // maximize and quietly resets the latch to the BOOT value — re-creating
    // exactly the bug below, with no grep signature. Nothing sets it today
    // (`grep set_window_maximized src/` is empty), which is precisely why the
    // invariant belongs here as one local assignment instead of as a
    // cross-file contract nobody will remember.
    property bool maximizedLatch: false
    property bool fullScreenLatch: false
    // A FUNCTION, not a derived property, and that is load-bearing. A
    // `readonly property` computed from `visibility` is dirtied by the very
    // notify signal `onVisibilityChanged` is connected to, and the handler
    // connection runs FIRST, so a read of that property INSIDE the handler is
    // one transition stale. Measured under Qt 6.11 (offscreen, real
    // transitions): the minimize was seen as "not transient" — clearing the
    // latch, the one thing the transient guard exists to prevent — and the
    // following restore as "transient", so the handler returned and never put
    // it back. A maximized session then closed as `window_maximized: false`
    // with the 2556x1436 maximized footprint written into the FLOATING
    // window_width/height (the Rust size gate lets it through precisely
    // because neither flag is set) — Slint #618, the regression
    // settings_qt::save_window_geometry's doc comment says must never happen.
    // A function has no cached value that can be stale: it computes from the
    // enum it is handed, at call time.
    function transientVisibility(vis) {
        return vis === Window.Minimized || vis === Window.Hidden
    }

    // Last size seen in a REAL windowed frame. The shutdown paths need it
    // because a close from the taskbar happens while MINIMIZED: `window.width`
    // is then whatever the WM left behind, and the debounce may still be
    // holding an unsaved drag that has nowhere left to be deferred to. Only
    // captured once armed, so the boot clamp can never enter it. Zero means
    // "never saw one this session" — below the app floor, so Rust drops the
    // size and still records the flags, which is the honest outcome.
    property real lastFloatingWidth: 0
    property real lastFloatingHeight: 0
    function captureFloatingSize() {
        if (!window.geometryArmed || window.visibility !== Window.Windowed)
            return
        // The enum alone is not enough. An un-maximize delivers the state
        // change and the geometry change in whichever order the platform
        // plugin happens to emit them (xdg_toplevel configure carries both), so
        // on a state-first compositor this runs with `visibility` already
        // Windowed while `width`/`height` are still the MAXIMIZED footprint —
        // and the cache would then hand the shutdown path a screen-sized
        // "floating" size, which Rust accepts because neither flag is set.
        // Reject any frame that is still the full screen; the real floating
        // frame arrives a moment later through onWidthChanged.
        var scr = window.screen
        if (scr && window.width >= scr.width - 1 && window.height >= scr.height - 1)
            return
        window.lastFloatingWidth = window.width
        window.lastFloatingHeight = window.height
    }

    // Persistence is ARMED late, on purpose. The restore clamp can hand the
    // window a SMALLER size than the file holds (a 2560px session opened on a
    // 1920px panel) and the WM may adjust the first frames too; persisting
    // those settling frames would lose the big-monitor size after one launch on
    // the small one — for the Slint build as well, since the file is shared.
    // Slint re-saves the clamped size by design ("the Resized handler re-saves
    // the result", main.rs:8208); this is the one place the Qt port
    // deliberately improves on it instead of matching it. No real user resize
    // happens inside the first 1.2s of a cold start, and anything the user does
    // after that persists normally.
    property bool geometryArmed: false
    Timer {
        id: geometryArmTimer
        interval: 1200
        running: true
        repeat: false
        onTriggered: window.geometryArmed = true
    }

    function persistWindowGeometry() {
        if (!window.geometryArmed)
            return
        // Transient: FLUSH the cached floating size, do not drop it and do not
        // defer it. Dropping was the original bug (a drag that debounced into a
        // minimize was lost; Slint writes synchronously from the winit Resized
        // handler and cannot lose it). Re-arming the timer instead was the
        // over-correction: the re-arm is itself what keeps the save
        // outstanding, so a window left minimized to the tray polls at 2.5 Hz
        // forever — measured at 15 wakeups in 6 s, i.e. ~72,000 over a workday.
        // Flushing the cache ends both problems in one line: `window.width` is
        // untrustworthy while iconified, but `lastFloatingWidth` is the last
        // REAL windowed frame, and Rust's floating-only + app-minimum + >0.5px
        // dirty gate turns a zero or stale cache into a no-op by itself.
        if (window.transientVisibility(window.visibility)) {
            QbzShell.saveWindowGeometry(window.lastFloatingWidth,
                                        window.lastFloatingHeight,
                                        window.maximizedLatch,
                                        window.fullScreenLatch)
            return
        }
        QbzShell.saveWindowGeometry(window.width,
                                    window.height,
                                    window.maximizedLatch,
                                    window.fullScreenLatch)
    }

    // Shutdown variant. Nothing can be deferred once the app is going away, so
    // instead of skipping the write, a transient window persists the last
    // floating size we actually saw. The flags travel either way — they are
    // what the restore reads (main.rs:8273-8281).
    function persistWindowGeometryOnExit() {
        // The 1.2s arming window guards the SIZE against boot-settling frames.
        // It must not swallow the FLAGS: a boot clamp never changes visibility,
        // so a maximize/restore inside the first 1.2s is a genuine user action,
        // and it is the flag — not the size — that both frontends read back
        // (main.rs:8273-8281). Persist the flags with a 0x0 size, which is
        // below the app floor, so Rust records them and drops the size.
        if (!window.geometryArmed) {
            QbzShell.saveWindowGeometry(0, 0,
                                        window.maximizedLatch,
                                        window.fullScreenLatch)
            return
        }
        if (!window.transientVisibility(window.visibility)) {
            window.persistWindowGeometry()
            return
        }
        QbzShell.saveWindowGeometry(window.lastFloatingWidth,
                                    window.lastFloatingHeight,
                                    window.maximizedLatch,
                                    window.fullScreenLatch)
    }

    Timer {
        id: geometrySaveTimer
        interval: 400
        repeat: false
        onTriggered: window.persistWindowGeometry()
    }

    onWidthChanged: { window.captureFloatingSize(); geometrySaveTimer.restart() }
    onHeightChanged: { window.captureFloatingSize(); geometrySaveTimer.restart() }
    // Maximize/restore/fullscreen: the flag itself is the thing that changed,
    // and the sizes that come with it are deliberately not persisted. A
    // minimize or a hide is NOT a state change to record — it is the same
    // window in a transient mode, so the latch keeps its last real value and
    // the debounce is not even restarted.
    onVisibilityChanged: {
        // Read the enum ONCE into a local and decide everything from it — see
        // `transientVisibility` for why nothing here may consult a property
        // derived from `visibility`.
        var vis = window.visibility
        if (window.transientVisibility(vis))
            return
        // AppKit RE-LAYS OUT the titlebar on fullscreen enter/exit and on the
        // maximize round trip, re-parking the traffic lights at their stock
        // 28pt-bar position. The reference records that drift and lives with
        // it ("the centre can drift until the next launch",
        // crates/qbz/src/macos_chrome.rs); Qt hands us this edge for free, so
        // re-centre instead of shipping it. Placed AFTER the transient
        // early-return on purpose — a minimize is not a re-layout, and while
        // the window is hidden AppKit has no main/key window, so the retry
        // loop would spin for its full 5s finding nothing.
        //
        // `applyMacChrome` is idempotent: already-centred lights return on the
        // half-point test without touching a frame.
        if (QbzShell.isMacos && !QbzShell.systemTitleBar) {
            macChromeTimer.attempts = 0
            macChromeTimer.restart()
        }
        // FLUSH BEFORE LATCHING. Leaving Windowed for Maximized/FullScreen with
        // a drag still in the debounce used to lose that drag outright: the
        // handler latched the new state and restarted the SAME timer, so when
        // it fired it handed Rust the post-transition size with maximized true
        // — and the floating-only gate refuses a size on exactly that
        // condition. The exit path could not rescue it either, since it passes
        // the same latches. Proven by effect: resize 1180->1600, maximize
        // 157ms later, quit — the file kept 1180x760 while Slint, for the same
        // event stream, kept 1600x900. Commit the floating frame we already
        // hold before the latches stop describing it.
        if (window.geometryArmed && geometrySaveTimer.running
                && !window.maximizedLatch && !window.fullScreenLatch
                && vis !== Window.Windowed) {
            geometrySaveTimer.stop()
            QbzShell.saveWindowGeometry(window.lastFloatingWidth,
                                        window.lastFloatingHeight,
                                        false, false)
        }
        // Maximized and fullscreen are ORTHOGONAL in winit, which is what
        // Slint persists. ASSUMED, NOT MEASURED: on X11
        // `_NET_WM_STATE_MAXIMIZED_*` is expected to survive a fullscreen
        // transition under Mutter/KWin, so `is_maximized()`
        // (main.rs:1404-1408) would keep reporting true from fullscreen. On
        // Wayland a compositor may instead send a states array carrying only
        // `fullscreen`, in which case winit reports false and the two
        // frontends would write OPPOSITE values into the shared
        // window_maximized. Settling this costs one measurement, not an
        // argument: run the Slint build, go maximized -> WM fullscreen, and log
        // `w.window().is_maximized()` from the Resized handler. If it reports
        // false, clear the maximized latch in the FullScreen branch too. Qt
        // collapses both into one enum, so the maximized latch is cleared ONLY
        // by an explicit return to Windowed — never by entering fullscreen.
        // FullScreen arrives from the WM keybind OR from the immersive
        // chrome (immersive/ImmersiveHeader.qml fs toggle — the first
        // FullScreen write in this frontend, 2026-08-02 §5.2); quitting from
        // there must not un-maximize the next launch of either frontend.
        if (vis === Window.Maximized) {
            window.maximizedLatch = true
            window.fullScreenLatch = false
        } else if (vis === Window.FullScreen) {
            window.fullScreenLatch = true
        } else if (vis === Window.Windowed) {
            window.maximizedLatch = false
            window.fullScreenLatch = false
        }
        // An un-maximize delivers its size change and its visibility change in
        // either order; capturing here as well as in onWidthChanged means the
        // floating cache is right whichever arrives second.
        window.captureFloatingSize()
        geometrySaveTimer.restart()
    }

    // The WM close / Alt+F4 exit, routed into the ONE close choreography
    // (A-26, below). It is the ONLY exit that delivers a close EVENT, and
    // `closeOrHide` refuses that event before hiding — §3.1 measured that
    // QGuiApplicationPrivate::maybeLastWindowClosed is reached from exactly two
    // sites, both inside QWindow::event and both gated on the accepted flag, so
    // hiding never arms Qt's quit check but an ACCEPTED close does.
    //
    // `onAboutToQuit` is still not a duplicate of it: neither `Qt.exit(0)`
    // (what every quit arm calls since 2026-08-04) nor `Qt.quit()` raises
    // `closing` (§15 trap 26), so the quit arm of `closeOrHide`, the tray's
    // `quit_requested` handler and any teardown that skips QML reach the
    // geometry flush only through here. Where both do run the second write is
    // free (Rust's >0.5px dirty check drops it), and either way a resize in the
    // last 400ms still lands, including the taskbar close of a minimized window
    // (that is what the exit variant and the floating cache are for).
    onClosing: function (close) { window.closeOrHide(close) }
    Connections {
        target: Qt.application
        function onAboutToQuit() { window.persistWindowGeometryOnExit() }
    }

    // --- Close-to-tray, and the ONE visibility owner (contract B6) ---------
    //
    // THE INVARIANT (§5.8.1, §13-D18, AC-24, stage-0 S0-4): the main window is
    // hidden and shown ONLY by these two functions, and they are the ONLY
    // callers of QbzTray.setWindowShown. The reference has a real desync
    // exactly here — the miniplayer's enter()/exit() call m.hide()/m.show()
    // directly (crates/qbz/src/miniplayer.rs:186, :207) instead of
    // tray::hide_window/show_window, so WINDOW_SHOWN goes stale and, while the
    // mini is up, the first tray left-click hides an already-hidden window and
    // the second brings the MAIN window back beside the live mini (#559/#618).
    // A bare window.hide()/window.show() anywhere in this file re-opens that
    // hole; the miniplayer handler below calls these verbs for that reason.
    //
    // ⚠ ONE WRITER THE S0-4 GREP CANNOT SEE, named here so a green grep is not
    // read as more than it proves: the drawn MINIMIZE buttons call
    // `hostWindow.showMinimized()` (shell/HeaderBar.qml, shell/KioskShell.qml)
    // and deliberately do NOT report. That is correct — a minimized window is
    // still SHOWN, not hidden-to-tray, and owner ruling K5 keeps that button an
    // unconditional WM minimize. The consequence is real and inherited from the
    // reference, which tracks the same flag the same way: with the window
    // ICONIFIED, `WINDOW_SHOWN` is still true, so the next tray left-click
    // takes the hide arm rather than restoring. `showFromTray()` below at least
    // un-iconifies, so the SECOND click genuinely brings it back.
    // The un-hide POSITION (owner report 2026-08-31). Qt keeps the window's
    // SIZE across hide()/show(), but a remap is a fresh mapping to the WM and
    // KWin's placement policy re-places it (observed: horizontally centred).
    // The persisted geometry cannot help — saveWindowGeometry carries only
    // width/height + flags, no x/y — so the position is captured at the hide
    // and re-applied around the show. Session-local on purpose: across
    // launches the WM's placement remains the authority, this only makes
    // hide -> restore a round trip. On Wayland the x/y writes are no-ops and
    // the behavior simply stays what it was.
    property real trayRestoreX: 0
    property real trayRestoreY: 0
    property bool trayRestoreValid: false

    function hideToTray() {
        // Only a real WINDOWED frame is worth restoring: a maximized/
        // fullscreen window is re-placed by its latch, and an iconified
        // window's x/y are whatever the WM left behind.
        window.trayRestoreValid = window.visibility === Window.Windowed
        window.trayRestoreX = window.x
        window.trayRestoreY = window.y
        // Both reference close arms flush the session BEFORE hiding, "even when
        // only hiding — the process may be killed from the tray / shell without
        // a real quit afterwards" (crates/qbz/src/main.rs:23255-23256). The Qt
        // session persistence itself is flushed by the Rust exit path; this
        // early geometry flush is still owed before a hide because the
        // process may later be killed without a QML close event. On a
        // miniplayer enter it is harmless: it writes the size the exit is
        // about to restore.
        window.persistWindowGeometryOnExit()
        window.hide()
        QbzTray.setWindowShown(false)
    }
    // Re-assert shortly after the map: setting x/y before show() seeds the
    // position hint, but a placement-policy WM can still apply its own frame
    // ON the map, after which one late write wins. One-shot, user-action
    // driven — not a continuous clock (repaint-pulse contract untouched).
    Timer {
        id: trayRestoreTimer
        interval: 50
        repeat: false
        onTriggered: {
            if (window.trayRestoreValid
                    && window.visibility === Window.Windowed) {
                window.x = window.trayRestoreX
                window.y = window.trayRestoreY
            }
        }
    }
    function showFromTray() {
        // Restore Regular activation policy before showing/focusing on macOS.
        QbzTray.setWindowShown(true)
        if (window.trayRestoreValid) {
            window.x = window.trayRestoreX
            window.y = window.trayRestoreY
        }
        window.show()
        // Un-iconify. Qt's hide() preserves windowStates, unlike the
        // reference's surface destruction (crates/qbz/src/tray/mod.rs:253-265),
        // so a window that was MINIMIZED before it was hidden comes back
        // minimized — visible in the taskbar, invisible on screen, which reads
        // as "the tray did nothing". Restoring to the maximized latch rather
        // than to Windowed keeps a maximized session maximized.
        if (window.visibility === Window.Minimized)
            window.visibility = window.maximizedLatch ? Window.Maximized
                                                      : Window.Windowed
        // raise() + requestActivate() is §13-D12, carried by the VERB so no
        // call site has to remember it: the reference's tray::show_window
        // focuses (crates/qbz/src/tray/mod.rs:226-229) while its miniplayer
        // exit() does not, and the port focuses on both paths.
        window.raise()
        window.requestActivate()
        if (window.trayRestoreValid)
            trayRestoreTimer.restart()
    }

    // THE ONE close choreography (A-26, §5.7). Five exits reach it: this
    // window's onClosing above, the drawn X and the app-menu Close row in
    // shell/HeaderBar.qml, the kiosk shell's own drawn X in
    // shell/KioskShell.qml, and the miniplayer's closeApp through the
    // QbzMini.closeAppRequested handler below.
    //
    // On macOS Close hides and Dock/Finder reopen restores the current window,
    // even without a menu-bar item. On other desktops BOTH a live tray and
    // close-to-tray are required, so hiding cannot strand an unreachable app.
    // Quit remains a separate command on every platform.
    //
    // `closeEvent` is the QQuickCloseEvent on the one path that carries one and
    // null on the other four.
    function closeOrHide(closeEvent) {
        if (closeEvent)
            closeEvent.accepted = false
        if (quitConfirmation.opened)
            return
        // macOS always has the Dock/Finder reopen path, even without a tray.
        var hide = QbzShell.isMacos || (QbzTray.trayLive && QbzTray.closeToTray)
        // The evidence line (2026-08-04): logged RUST-side so it lands in
        // qbz.log — console.log only reaches stderr, which owner reports
        // never carry. It prints both operands and the arm taken, which is
        // the whole diagnosis of any future "Close to tray does nothing".
        QbzTray.closeDecision(hide)
        if (hide) {
            if (closeEvent)
                closeEvent.accepted = false
            window.hideToTray()
            return
        }
        // QUIT — via Qt.exit(0), NEVER Qt.quit(). Qt.quit() delivers
        // QEvent::Quit, and QGuiApplication's handler first closes every
        // top-level window, silently CANCELING the whole quit if any window
        // refuses its close — this very function's hide arm refuses whenever
        // the tray is live with close-to-tray on, and MiniWindow.qml's
        // onClosing refuses unconditionally (its close means "exit the
        // mini"). That veto is how "Quit QBZ" hid the window and left the
        // process running (owner-reported three times; reproduced under VNC
        // 2026-08-04: quit requested + Qt.quit() both logged, event loop
        // still alive 25s later). Qt.exit() stops the event loops directly:
        // no close events, no veto. Geometry is flushed explicitly below,
        // and onAboutToQuit still runs when exec() returns.
        requestQuit()
    }

    property bool quitAccepted: false
    function requestQuit() {
        if (JSON.parse(QbzAbout.updatesJson).phase === "installing") {
            QbzAbout.updatesCheck()
            return
        }
        if (quitAccepted || quitConfirmation.opened)
            return
        if (!QbzTray.confirmQuitEnabled()) {
            finishQuit()
            return
        }
        if (!window.visible || window.visibility === Window.Minimized)
            window.showFromTray()
        window.raise()
        window.requestActivate()
        quitConfirmation.checkboxChecked = false
        quitConfirmation.open()
    }

    function finishQuit() {
        // The download may have reached replacement while a quit confirmation
        // was open. Check again before arming the hard-exit watchdog.
        if (JSON.parse(QbzAbout.updatesJson).phase === "installing") {
            QbzAbout.updatesCheck()
            return
        }
        if (quitAccepted)
            return
        quitAccepted = true
        QbzTray.armQuitWatchdog()
        window.persistWindowGeometryOnExit()
        Qt.exit(0)
    }

    QbzConfirmModal {
        id: quitConfirmation
        anchors.fill: parent
        title: QbzSession.tr("Close QBZ?", QbzSession.trRev)
        cancelLabel: QbzSession.tr("No", QbzSession.trRev)
        confirmLabel: QbzSession.tr("Yes", QbzSession.trRev)
        checkboxLabel: QbzSession.tr("Don't show again", QbzSession.trRev)
        danger: false
        onCancelled: {
            if (screenLoader.item)
                screenLoader.item.forceActiveFocus()
        }
        onConfirmed: {
            if (checkboxChecked)
                QbzTray.disableQuitConfirmation()
            window.finishQuit()
        }
    }

    // The tray's four signals (src/tray_bridge.rs:87-100). Rust owns no window
    // (§2.3), so every tray click hops ksni thread -> tray_qt verb ->
    // tray_bridge::ui() -> qsignal -> here, and this block is where it finally
    // reaches one.
    // Each handler logs ONE line, and that is not debug litter: this hop is the
    // only part of the tray that Rust cannot see. `tray_qt` logs when it SENDS
    // (e.g. "[tray] quit requested"), so without a line on this side a report of
    // "the tray does nothing" cannot distinguish "the click never left ksni"
    // from "the signal arrived and QML did nothing with it" — which is exactly
    // the ambiguity that cost a diagnosis on 2026-08-04.
    Connections {
        target: QbzTray
        function onCloseRequested() { window.closeOrHide(null) }
        function onWindowShowRequested() {
            if (window.quitAccepted)
                return
            console.log("[qml] tray -> showFromTray")
            window.showFromTray()
        }
        function onWindowHideRequested() {
            console.log("[qml] tray -> hideToTray")
            window.hideToTray()
        }
        // The tray's left-click degenerates to present() while the mini is open
        // (§5.8.2, §13-D21): raise the MINI, and do NOT bring the main window up
        // beside it. No setWindowShown — the mini is not the main window, and
        // the flag still honestly says the main one is hidden.
        function onMiniPresentRequested() {
            if (window.quitAccepted)
                return
            var mini = window.ensureMiniWindow()
            if (mini !== null) {
                mini.show()
                if (mini.visibility === Window.Minimized)
                    mini.showNormal()
                mini.raise()
                mini.requestActivate()
            }
        }
        function onQuitRequested() {
            window.requestQuit()
        }
    }

    // --- The miniplayer window (2026-08-03 miniplayer/tray contract, B2) ---
    //
    // LAZY, with a STRONG reference. Declaring the mini inline would build its
    // whole subtree at startup; a Component + createObject(null) is the Qt
    // shape of the reference's ensure_window() (crates/qbz/src/miniplayer.rs:
    // 121-151), and `miniWindow` is its MINI_STRONG thread-local (:40 "Strong
    // handle keeps the window alive", set :149, NEVER cleared — exit() and
    // close_app() only hide()). A null-parent createObject with no strong
    // reference is JS-GC eligible (contract §16 U-3), and this property is what
    // makes it not.
    //
    // Created on first ENTER, never on boot: the QML tree of a window that has
    // never been opened costs nothing.
    property var miniWindow: null
    Component {
        id: miniWindowComponent
        MiniWindow {}
    }
    function ensureMiniWindow() {
        if (window.miniWindow === null)
            window.miniWindow = miniWindowComponent.createObject(null)
        return window.miniWindow
    }

    // `QbzMini.open` is the ONE mini lifecycle flag (contract A-2): Rust's
    // enter/exit funnel writes it, stores the AtomicBool mirror the hotkeys
    // pipeline reads, and THEN notifies — so this handler is the QML half of
    // §4.6's step ordering and the only place either window is shown or hidden
    // for the miniplayer.
    //
    // Step 7 of enter() is "hide the main window only after the mini is visible
    // (avoid a blank flash)" (crates/qbz/src/miniplayer.rs:185-187), so
    // hideToTray() runs LAST here and showFromTray() runs after the mini is
    // down. They are the A-25 verbs, never a bare hide()/show(): the mini IS
    // where the reference's WINDOW_SHOWN desync comes from, and reporting
    // shown=false/true from these two paths is what makes the tray toggle
    // honest (§13-D18, stage-0 S0-4).
    //
    // Re-entering while already open forces an openChanged (mini_bridge.rs's
    // enter_now), so this runs again and raises the mini; hideToTray() on an
    // already-hidden window is a no-op plus one geometry write Rust's dirty
    // check drops.
    Connections {
        target: QbzMini
        function onOpenChanged() {
            if (QbzMini.open) {
                var mini = window.ensureMiniWindow()
                if (mini !== null) {
                    mini.show()
                    mini.raise()
                    mini.requestActivate()
                }
                window.hideToTray()
            } else if (window.miniWindow !== null) {
                window.miniWindow.hide()
                window.showFromTray()
            }
        }
        // The mini's own close verb (A-12). QbzMini.closeApp() exits the mini
        // FIRST — which runs the handler above and brings the main window back
        // — and only then emits this, so the app's ONE close choreography
        // decides between hide-to-tray and quit. The brief show-then-hide is
        // 1:1 with the reference: miniplayer.rs:235-251 hides the mini, shows
        // the main window and restores its geometry before calling
        // invoke_close_app, because "the window becomes user-visible when
        // close-to-tray keeps the app alive" (:244-246).
        function onCloseAppRequested() { window.closeOrHide(null) }
    }

    // The immersive toggle Shortcut is GONE (2026-08-03 hotkeys-port §1.3):
    // Shift+I now fires as the REBINDABLE ui.focusMode action through the
    // AppShell dispatcher (QbzHotkeys.keyPressed), whose central text-input
    // gate replaces the `enabled:` expression this block carried.
    // NavGestureLayer.qml owns mouse Back/Forward; no other Shortcut item
    // exists in the tree (trap 11).

    Loader {
        id: screenLoader
        anchors.fill: parent
        active: QbzSession.screen !== "splash"
        // "kiosk" is a SIBLING of "shell", not a mode inside it — the same
        // shape as the reference (app.slint:38's AppScreen enum, with the two
        // shells as sibling mounts at :145 and :205). Both read the same
        // bridges, so the live toggle swaps them with nothing torn down.
        // An unknown id falls through to "" and the Loader goes blank, which
        // is the documented failure mode for a missing route (nav_qt.rs:14-16)
        // and is what a missing KioskShell.qml would look like.
        source: QbzSession.screen === "login"
                ? (QbzShell.kioskProfile ? "" : "LoginScreen.qml")
                : (QbzSession.screen === "shell" ? "shell/AppShell.qml"
                   : (QbzSession.screen === "kiosk" ? "shell/KioskShell.qml" : ""))
        // Hand the host window down for drag/maximize/resize (custom chrome).
        onLoaded: if (screenLoader.item) screenLoader.item.hostWindow = window
    }

    // The appliance login opts into compact scrollable layout at construction.
    Loader {
        anchors.fill: parent
        active: QbzSession.screen === "login" && QbzShell.kioskProfile
        sourceComponent: LoginScreen { kioskHost: true; hostWindow: window }
    }

    // Compact per-track metadata editor. It lives above the routed shell so a
    // successful save can republish Local Library without destroying the
    // modal halfway through its own transaction.
    Loader {
        anchors.fill: parent
        active: QbzTagEditor.trackEditorOpen
        source: "controls/TrackMetadataModal.qml"
        z: 4000
    }

    // Local media info is a GLOBAL action target: rows can open it from the
    // library tabs, an album detail, search, or any other routed page. Keeping
    // the modal above the screen Loader also means a route change cannot be
    // required to mount it (or destroy it while it is resolving metadata).
    LocalMediaInfoModal { }

    // Frameless hairline (app.slint's no-frame 1px edge) — paints at the
    // very rim, over everything. SQUARE (the app draws no corner rounding).
    Rectangle {
        anchors.fill: parent
        color: "transparent"
        border.width: 1
        border.color: "#14ffffff"
        visible: !QbzShell.systemTitleBar
            && window.visibility !== Window.Maximized && window.visibility !== Window.FullScreen
    }

    // Edge/corner resize grips (custom chrome — the compositor draws no
    // border). 6px, invisible.
    MouseArea {
        anchors.left: parent.left; anchors.top: parent.top; anchors.bottom: parent.bottom; width: 6
        cursorShape: Qt.SizeHorCursor
        enabled: !QbzShell.systemTitleBar
        onPressed: window.startSystemResize(Qt.LeftEdge)
    }
    MouseArea {
        anchors.right: parent.right; anchors.top: parent.top; anchors.bottom: parent.bottom; width: 6
        cursorShape: Qt.SizeHorCursor
        enabled: !QbzShell.systemTitleBar
        onPressed: window.startSystemResize(Qt.RightEdge)
    }
    MouseArea {
        anchors.left: parent.left; anchors.right: parent.right; anchors.top: parent.top; height: 6
        cursorShape: Qt.SizeVerCursor
        enabled: !QbzShell.systemTitleBar
        onPressed: window.startSystemResize(Qt.TopEdge)
    }
    MouseArea {
        anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom; height: 6
        cursorShape: Qt.SizeVerCursor
        enabled: !QbzShell.systemTitleBar
        onPressed: window.startSystemResize(Qt.BottomEdge)
    }
    MouseArea {
        anchors.left: parent.left; anchors.top: parent.top; width: 12; height: 12
        cursorShape: Qt.SizeFDiagCursor
        enabled: !QbzShell.systemTitleBar
        onPressed: window.startSystemResize(Qt.TopEdge | Qt.LeftEdge)
    }
    MouseArea {
        anchors.right: parent.right; anchors.top: parent.top; width: 12; height: 12
        cursorShape: Qt.SizeBDiagCursor
        enabled: !QbzShell.systemTitleBar
        onPressed: window.startSystemResize(Qt.TopEdge | Qt.RightEdge)
    }
    MouseArea {
        anchors.left: parent.left; anchors.bottom: parent.bottom; width: 12; height: 12
        cursorShape: Qt.SizeBDiagCursor
        enabled: !QbzShell.systemTitleBar
        onPressed: window.startSystemResize(Qt.BottomEdge | Qt.LeftEdge)
    }
    MouseArea {
        anchors.right: parent.right; anchors.bottom: parent.bottom; width: 12; height: 12
        cursorShape: Qt.SizeFDiagCursor
        enabled: !QbzShell.systemTitleBar
        onPressed: window.startSystemResize(Qt.BottomEdge | Qt.RightEdge)
    }

    // Splash (SplashScreen.slint): the same 720px dark card as the login
    // screen while the silent session restore resolves.
    //
    // PARITY PASS (2026-08-19). Three things this block had lost against the
    // reference, all of them user-visible:
    //   * the LEGAL DISCLAIMER, which SplashScreen.slint:75-86 pins to the
    //     bottom of the SCREEN (not inside the card — Tauri put it there and
    //     the Slint port kept that on purpose). It was simply absent here, so
    //     the one screen a first-run user stares at said nothing about the
    //     app not being certified by Qobuz.
    //   * the strings, which were bare QML literals. Every other screen goes
    //     through QbzSession.tr() with the EXACT msgid of the Slint @tr()
    //     call so the existing .po catalogues apply; these three did not, so
    //     the splash was English in all eight locales.
    //   * the colours and metrics, which were hardcoded hex and magic numbers
    //     (#0f0f0f / #1a1a1a / #ffffff / #888888, 52, 104, 8, 32). They happen
    //     to match the DEFAULT dark palette and nothing else: on any of the
    //     other 34 themes the splash was a black card the rest of the app had
    //     stopped being. Tokens now, like LoginScreen.qml.
    Rectangle {
        anchors.fill: parent
        color: splashTheme.surfaceMain
        visible: QbzSession.screen === "splash"

        QbzTheme { id: splashTheme }

        // Faked drop shadow (blur 32, offset-y 8) — the same hand-copy
        // LoginScreen.qml:41-49 carries, for the same reason and with the same
        // caveat written there. The reference gives BOTH cards the shadow
        // (SplashScreen.slint:23-25); only the login one had it here.
        Rectangle {
            anchors.horizontalCenter: splashCard.horizontalCenter
            y: splashCard.y + 8
            width: splashCard.width
            height: splashCard.height
            radius: splashTheme.radiusLg
            color: splashTheme.cardShadow
            opacity: 0.5
        }

        Rectangle {
            id: splashCard
            anchors.centerIn: parent
            width: 720
            height: splashColumn.height + 2 * splashTheme.cardPadding
            radius: splashTheme.radiusLg
            color: splashTheme.surfaceCard

            Column {
                id: splashColumn
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                anchors.margins: splashTheme.cardPadding
                spacing: 0
                Image {
                    anchors.horizontalCenter: parent.horizontalCenter
                    source: "assets/qbz-logo.png"
                    width: 140
                    height: 140
                    fillMode: Image.PreserveAspectFit
                }
                Item { width: 1; height: splashTheme.spacingSm }
                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    text: QbzSession.tr("QBZ", QbzSession.trRev)
                    color: splashTheme.textPrimary
                    font.pixelSize: splashTheme.fontWordmark
                    font.weight: splashTheme.weightSemibold
                    font.letterSpacing: 8
                }
                Item { width: 1; height: 2 }
                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    text: QbzSession.tr("QOBUZ™ PLAYER", QbzSession.trRev)
                    color: splashTheme.textMuted
                    font.pixelSize: splashTheme.fontSubtitle
                    font.letterSpacing: 4
                }
                Item { width: 1; height: splashTheme.spacingXl }
                QbzSpinner {
                    anchors.horizontalCenter: parent.horizontalCenter
                    size: 30
                }
            }
        }

        // Legal disclaimer — pinned to the BOTTOM OF THE SCREEN, not inside
        // the card. That placement is the reference's, and its comment says it
        // is Tauri's too (SplashScreen.slint:73-74). Two msgids concatenated
        // with a space, exactly as the reference concatenates them, so both
        // reuse the catalogue entries the login screen already uses.
        Text {
            width: Math.min(parent.width - 80, 720)
            x: Math.round((parent.width - width) / 2)
            y: parent.height - height - 24
            text: QbzSession.tr("This application uses the Qobuz API but is not certified by Qobuz.", QbzSession.trRev)
                + " "
                + QbzSession.tr("Qobuz™ is a trademark of Qobuz. QBZ is an open-source application licensed under the MIT License and is not affiliated with, endorsed by, or certified by Qobuz.", QbzSession.trRev)
            color: splashTheme.textMuted
            // 10px LITERAL, not fontLegal: the reference hardcodes 10px here
            // (SplashScreen.slint:80) where its login twin uses Typography.legal.
            font.pixelSize: 10
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.WordWrap
        }
    }
}
