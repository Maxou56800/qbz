// ArtistScene › the genre-filter popup — 1:1 with Tauri's `.genre-popup`
// (ArtistsByLocationView.svelte markup :495-549, CSS :1128-1347), per
// 00-CONTRACT.md §0 (Tauri wins on every number) and 01-tauri-artist-scene.md
// §6, which is normative here.
//
// NOT the same component as controls/GenreFilterPopup.qml. That one is the
// Discover/Library overlay: 470px wide, driven end-to-end by
// QbzBridge.genreFilterJson, with a remember/advanced toggle pair and a
// 3-level genre TREE. This one is a dumb, self-contained box over the genre
// strings the scene's own candidates carry — no bridge, no tree, no
// persistence, and a fixed 3-column grid of 36px cards. Same job title, two
// different surfaces; the reference has two files as well.
//
// ⚠ TWO COMPONENTS NOW SHARE THE NAME `GenreFilterPopup`, and ArtistSceneView
// imports BOTH directories ("../controls" and "scene"). QML resolves a type by
// walking the imports in reverse, so "scene" — imported last — wins today and
// the right box is drawn. It is still one reordered import away from silently
// mounting the 470px Discover tree instead. The fix belongs to the integration
// lane and is three tokens: `import "scene" as Scene`, then `Scene.` on the
// three mounts. Flagged in this lane's report.
//
// MOUNTING — the root IS the 530px card, positioned by the host, which is what
// ArtistSceneView.qml:1236-1242 already does:
//
//       GenreFilterPopup {
//           z: 200
//           open: root.genrePopupOpen
//           x: … genreBtn.x + genreBtn.width - width      // `right: 0`
//           y: … genreBtn.y + genreBtn.height + 6         // `calc(100% + 6px)`
//           genres: root.availableGenres                  // ["post-punk", …]
//           selected: root.activeGenres                   // array OR {g: true}
//           onToggled: function (g) { … }
//           onCleared: function () { … }
//           onCloseRequested: root.genrePopupOpen = false
//       }
//
// Tauri anchors the card to the 36×36 funnel button (`.genre-filter-container
// {position:relative}` :1129-1131, which 01-…md §6 omits and 08-critique
// records), i.e. `top: calc(100% + 6px); right: 0` of THAT button — NOT of the
// nav bar and NOT of the view.
//
// OUTSIDE CLICK IS THE HOST'S. Tauri closes on a document-level handler
// (V:240-246); the QML equivalent is a full-surface scrim, which this card
// cannot be and still be positioned as a 530px box. ArtistSceneView already
// owns that scrim (`outsideScrim`, :1168-1177, one scrim for this popup and the
// group menu — its DV-4 fix). What this component contributes is the swallow
// MouseArea below, without which a press INSIDE the card would fall through to
// that scrim and close the popup on every genre click.
//
// SELECTION IS THE HOST'S. This box only reports `toggled(genre)` and
// `cleared()`; the AND semantics of the filter chain (V:250-262 — an artist is
// kept only when it carries EVERY selected genre, case-sensitive) live in the
// consumer.

import QtQuick
import "../../controls"
import com.blitzfc.qbz
import "../../theme"

Item {
    id: root

    // --- the frozen mount API (00-CONTRACT.md, scene lane) ---------------
    /// Every genre string the scene's candidates carry, already deduped by
    /// the host. Order is the host's; this box does not sort.
    property var genres: []
    /// The active selection. Accepts an array (["post-punk"]) or a
    /// set-shaped object ({"post-punk": true}) — Tauri's is a JS Set and the
    /// host may hold either.
    property var selected: []
    /// Visibility. Named `open` to match the mount contract.
    property bool open: false

    signal toggled(string genre)
    signal cleared()
    signal closeRequested()

    QbzTheme { id: theme }

    // `width: 530px; max-height: 440px` (:1136-1137). The card is a flex
    // column whose only flexible band is the grid, so it is as tall as its
    // content up to 440.
    readonly property int headerH: 52
    readonly property int searchH: 41
    readonly property int footerH: 57
    width: 530
    height: Math.min(440, root.headerH + (root.searchShown ? root.searchH : 0)
                          + gridBand.contentH + root.footerH)

    // Tauri's z-index:200 (`.genre-popup` :1145). This is a view-local popup,
    // not a modal, so ADR-009's ≥3000 floor does not apply — that floor is for
    // surfaces that must clear the shell chrome, and this one must not. The
    // host sets it too; this is the default for any other mount.
    z: 200
    visible: root.open
    enabled: root.open

    // The search field is rendered only above 9 genres (V:508) — below that
    // the 3-column grid shows everything without scrolling and the row would
    // be dead chrome.
    readonly property bool searchShown: (root.genres || []).length > 9

    /// Selection test that survives both shapes of `selected`.
    function isSelected(g) {
        var s = root.selected
        if (!s)
            return false
        if (Array.isArray(s))
            return s.indexOf(g) >= 0
        return s[g] === true
    }
    readonly property int selectedCount: {
        var s = root.selected
        if (!s)
            return 0
        if (Array.isArray(s))
            return s.length
        var n = 0
        for (var k in s)
            if (s[k] === true)
                n++
        return n
    }

    // `.genre-name::first-letter { text-transform: capitalize }` (:1276-1278)
    // — the FIRST LETTER ONLY, not CSS `capitalize`, which would title-case
    // every word ("post punk" -> "Post punk", never "Post Punk"). QML has no
    // text-transform at all, so it happens here.
    function capFirst(s) {
        var t = String(s || "")
        return t.length === 0 ? t : t.charAt(0).toUpperCase() + t.slice(1)
    }

    // Closing resets the needle, as Tauri does on every close path
    // (V:242-245, V:519, V:542).
    onOpenChanged: if (!root.open) searchInput.text = ""

    readonly property var shownGenres: {
        var all = root.genres || []
        var q = searchInput.text.toLowerCase()
        if (!root.searchShown || q === "")
            return all
        var out = []
        for (var i = 0; i < all.length; i++)
            if (String(all[i]).toLowerCase().indexOf(q) >= 0)
                out.push(all[i])
        return out
    }

    // ============================== the card =============================
    // `box-shadow: 0 8px 32px rgba(0,0,0,0.4)` (:1144), approximated by four
    // concentric frames that fade outward. Declared BEFORE the card so the
    // card's own fill covers them — a negative-z CHILD would not work: Qt
    // Quick draws an item's own geometry before any child, whatever its z, so
    // the frames would have painted dark bars across the card's top edge.
    //
    // Qt.rgba with 0..1 floats, NEVER an 8-digit hex: Qt reads #AARRGGBB
    // where CSS and Slint write #RRGGBBAA, so "#00000066" is a fully
    // TRANSPARENT black here. That trap has landed twice in this port.
    Repeater {
        model: 4
        delegate: Rectangle {
            required property int index
            x: -(index + 1) * 2
            y: 8 - (index + 1) * 2
            width: root.width + (index + 1) * 4
            height: root.height + (index + 1) * 4
            radius: card.radius + (index + 1) * 2
            color: "transparent"
            border.width: 2
            border.color: Qt.rgba(0, 0, 0, 0.4 / (index + 2))
        }
    }

    Rectangle {
        id: card

        anchors.fill: parent
        radius: 10
        color: theme.surfaceMain
        border.width: 1
        border.color: theme.borderSubtle

        // Swallow presses on the card's own chrome (Tauri's
        // `onclick={e => e.stopPropagation()}`, V:498). Declared FIRST so the
        // content below keeps its own clicks.
        MouseArea { anchors.fill: parent }

        // ---------------------------- header -----------------------------
        Item {
            id: header
            width: parent.width
            height: root.headerH

            Row {
                x: 16
                anchors.verticalCenter: parent.verticalCenter
                spacing: 8
                QbzIcon {
                    name: "sliders-horizontal"
                    width: 16
                    height: 16
                    anchors.verticalCenter: parent.verticalCenter
                    tintName: "textPrimary"
                }
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: QbzSession.tr("Filter by genre", QbzSession.trRev)
                    color: theme.textPrimary
                    font.pixelSize: 14
                    font.weight: theme.weightSemibold
                }
            }

            Rectangle {
                x: parent.width - width - 16
                anchors.verticalCenter: parent.verticalCenter
                width: 28
                height: 28
                radius: 6
                color: closeArea.containsMouse ? theme.bgHover : "transparent"
                // Hover-only, gesture-bounded — the one animation class the
                // pulse law leaves alone (cards/ArtistCard.qml:170).
                Behavior on color { ColorAnimation { duration: 150 } }
                QbzIcon {
                    anchors.centerIn: parent
                    name: "x"
                    width: 16
                    height: 16
                    tintName: closeArea.containsMouse ? "textPrimary" : "muted"
                }
                MouseArea {
                    id: closeArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.closeRequested()
                }
            }

            Rectangle {
                y: parent.height - 1
                width: parent.width
                height: 1
                color: theme.borderSubtle
            }
        }

        // -------------------------- search row ---------------------------
        Item {
            id: searchRow
            visible: root.searchShown
            y: header.height
            width: parent.width
            height: root.searchH

            Row {
                x: 16
                width: parent.width - 32
                anchors.verticalCenter: parent.verticalCenter
                spacing: 8

                QbzIcon {
                    name: "search"
                    width: 14
                    height: 14
                    anchors.verticalCenter: parent.verticalCenter
                    tintName: "muted"
                }
                TextInput {
                    QbzTextEditMenu { }
                    id: searchInput
                    width: parent.width - 22 - (clearBtn.visible ? clearBtn.width + 8 : 0)
                    height: 21
                    anchors.verticalCenter: parent.verticalCenter
                    color: theme.textPrimary
                    font.pixelSize: 13
                    verticalAlignment: Text.AlignVCenter
                    clip: true
                    // Reusing the catalogue's existing needle msgid rather
                    // than Tauri's "Search..." — 00-CONTRACT.md §7: reuse the
                    // translated entry unless the owner asks otherwise.
                    Text {
                        visible: searchInput.text === ""
                        anchors.fill: parent
                        text: QbzSession.tr("Search…", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: 13
                        verticalAlignment: Text.AlignVCenter
                    }
                }
                Rectangle {
                    id: clearBtn
                    visible: searchInput.text !== ""
                    width: 20
                    height: 20
                    radius: 10
                    anchors.verticalCenter: parent.verticalCenter
                    color: clearArea.containsMouse ? theme.bgHover : theme.surfaceElevated
                    Behavior on color { ColorAnimation { duration: 150 } }
                    QbzIcon {
                        anchors.centerIn: parent
                        name: "x"
                        width: 12
                        height: 12
                        tintName: clearArea.containsMouse ? "textPrimary" : "muted"
                    }
                    MouseArea {
                        id: clearArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: searchInput.text = ""
                    }
                }
            }

            Rectangle {
                y: parent.height - 1
                width: parent.width
                height: 1
                color: theme.borderSubtle
            }
        }

        // ------------------------------ grid ------------------------------
        // `repeat(3, 1fr)`, gap 6, padding 12 — a FIXED 3 columns with no
        // breakpoint (:1228-1235), so the cell width is arithmetic, not a
        // layout negotiation.
        Item {
            id: gridBand

            readonly property int pad: 12
            readonly property int gap: 6
            readonly property int cellW: Math.floor((root.width - 2 * gridBand.pad
                                                     - 2 * gridBand.gap) / 3)
            readonly property int contentH: 2 * gridBand.pad
                + (root.shownGenres.length > 0 ? grid.height : emptyLabel.height)

            y: header.height + (root.searchShown ? searchRow.height : 0)
            width: parent.width
            height: root.height - y - root.footerH
            clip: true

            Flickable {
                anchors.fill: parent
                contentWidth: width
                contentHeight: gridBand.contentH
                boundsBehavior: Flickable.StopAtBounds
                clip: true

                Grid {
                    id: grid
                    visible: root.shownGenres.length > 0
                    x: gridBand.pad
                    y: gridBand.pad
                    columns: 3
                    columnSpacing: gridBand.gap
                    rowSpacing: gridBand.gap

                    Repeater {
                        model: root.shownGenres
                        delegate: Rectangle {
                            id: genreCard
                            required property var modelData

                            readonly property bool picked: root.isSelected(genreCard.modelData)

                            width: gridBand.cellW
                            height: 36
                            radius: 6
                            border.width: 1
                            color: genreCard.picked
                                ? (cardArea.containsMouse ? theme.accentHover : theme.accent)
                                : (cardArea.containsMouse ? theme.bgHover : theme.surfaceElevated)
                            border.color: genreCard.picked
                                ? (cardArea.containsMouse ? theme.accentHover : theme.accent)
                                : (cardArea.containsMouse ? theme.textMuted : theme.borderSubtle)
                            // `transition: color/background-color/border-color
                            // 150ms` (:1244-1247). Both arms of this colour are
                            // gesture-bounded — hover, and a click on THIS card
                            // (or the footer's Clear) is the only thing that can
                            // flip `picked`. It is not a data feed in the sense
                            // the pulse law forbids.
                            Behavior on color { ColorAnimation { duration: 150 } }
                            Behavior on border.color { ColorAnimation { duration: 150 } }

                            Text {
                                x: 10
                                width: parent.width - 10 - 14 - 10 - 8
                                height: parent.height
                                text: root.capFirst(genreCard.modelData)
                                // On the accent fill the label is the measured
                                // on-accent colour, not Tauri's
                                // `--btn-primary-text`: the same call the
                                // sibling popup already makes
                                // (controls/GenreFilterPopup.qml:429-430) —
                                // accent-text on 34 of the 35 palettes, and a
                                // legible colour on the one where it is not.
                                color: genreCard.picked ? theme.accentGlyphColor : theme.textPrimary
                                font.pixelSize: 11
                                font.weight: theme.weightMedium
                                verticalAlignment: Text.AlignVCenter
                                elide: Text.ElideRight
                            }

                            // `.check-circle` (:1284-1310) — 14px ring that
                            // fills white and grows a CSS-drawn tick. The tick
                            // is a 4×7 box wearing only its right and bottom
                            // borders, rotated 45°: two Rectangles here,
                            // because QML has no border-width shorthand and an
                            // icon glyph would not be the same mark.
                            Item {
                                id: checkCircle
                                x: parent.width - 10 - width
                                anchors.verticalCenter: parent.verticalCenter
                                width: 14
                                height: 14

                                Rectangle {
                                    anchors.fill: parent
                                    radius: 7
                                    // INVERTED host: the disc is the card's
                                    // foreground punched out of the accent
                                    // fill and the tick inside it is `accent`,
                                    // so the pair that has to clear the
                                    // contrast floor is (accent, this fill) —
                                    // exactly the swap
                                    // controls/GenreFilterPopup.qml:436-455
                                    // documents. Tauri paints it literal white.
                                    color: genreCard.picked ? theme.accentGlyphColor : "transparent"
                                    border.width: 1.5
                                    border.color: genreCard.picked
                                        ? theme.accentGlyphColor : theme.textMuted
                                    Behavior on color { ColorAnimation { duration: 150 } }
                                }

                                // `transform: translate(-50%,-60%) rotate(45deg)`
                                // off `top:50%; left:50%` — the translate is of
                                // the tick's OWN 4×7 box, the rotation is about
                                // its centre.
                                Item {
                                    visible: genreCard.picked
                                    x: parent.width * 0.5 - width * 0.5
                                    y: parent.height * 0.5 - height * 0.6
                                    width: 4
                                    height: 7
                                    rotation: 45
                                    transformOrigin: Item.Center
                                    // border-width: 0 1.5px 1.5px 0 — right
                                    // edge…
                                    Rectangle {
                                        x: parent.width - 1.5
                                        width: 1.5
                                        height: parent.height
                                        color: theme.accent
                                    }
                                    // …and bottom edge.
                                    Rectangle {
                                        y: parent.height - 1.5
                                        width: parent.width
                                        height: 1.5
                                        color: theme.accent
                                    }
                                }
                            }

                            MouseArea {
                                id: cardArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: root.toggled(String(genreCard.modelData))
                            }
                        }
                    }
                }

                // `.genre-popup-empty` (:1341-1347) — spans the whole grid,
                // centred, 12px muted, 16px of air above and below.
                Text {
                    id: emptyLabel
                    visible: root.shownGenres.length === 0
                    x: gridBand.pad
                    y: gridBand.pad
                    width: root.width - 2 * gridBand.pad
                    height: 16 + 16 + 16
                    text: QbzSession.tr("No results found", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: 12
                    horizontalAlignment: Text.AlignHCenter
                    verticalAlignment: Text.AlignVCenter
                }
            }
        }

        // ----------------------------- footer -----------------------------
        Item {
            id: footer
            y: root.height - root.footerH
            width: parent.width
            height: root.footerH

            Rectangle {
                width: parent.width
                height: 1
                color: theme.borderSubtle
            }

            Rectangle {
                x: 16
                y: 12
                width: parent.width - 32
                height: 32
                radius: 6
                // `:disabled { opacity: 0.4 }` (:1337-1339).
                opacity: root.selectedCount > 0 ? 1.0 : 0.4
                color: (clearFilterArea.containsMouse && root.selectedCount > 0)
                    ? theme.bgHover : theme.surfaceElevated
                Behavior on color { ColorAnimation { duration: 150 } }

                Text {
                    anchors.centerIn: parent
                    text: QbzSession.tr("Clear filter", QbzSession.trRev)
                    color: (clearFilterArea.containsMouse && root.selectedCount > 0)
                        ? theme.textPrimary : theme.textSecondary
                    font.pixelSize: 12
                    font.weight: theme.weightMedium
                }
                MouseArea {
                    id: clearFilterArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: root.selectedCount > 0 ? Qt.PointingHandCursor : Qt.ArrowCursor
                    // Tauri clears AND closes (V:542).
                    onClicked: {
                        if (root.selectedCount === 0)
                            return
                        root.cleared()
                        root.closeRequested()
                    }
                }
            }
        }
    }
}
