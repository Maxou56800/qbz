// Settings > Navigation — the navigation and search preferences split out
// of Appearance, which now keeps only the visual groups. Same settingsJson
// document, so every control keeps working unchanged; only the parent moved.
import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Column {
    property bool kioskHost: false
    id: root
    property var doc: ({})

    // ============================ NAVIGATION ============================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("NAVIGATION", QbzSession.trRev) }

    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Show navigation in sidebar", QbzSession.trRev)
        description: QbzSession.tr("Move the Discover, Library, Local Library and My QBZ sections out of the header and into the sidebar.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.navInSidebar === true
            onToggled: function (v) { QbzBridge.settingsBool("nav-in-sidebar", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        // ADR-010: only mounted when navigation is NOT in the sidebar.
        visible: root.doc.navInSidebar !== true
        label: QbzSession.tr("Compact header navigation", QbzSession.trRev)
        description: QbzSession.tr("Use the icon-only section navigation in the header even while the sidebar is open.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.navHeaderCompact === true
            onToggled: function (v) { QbzBridge.settingsBool("nav-header-compact", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("My QBZ", QbzSession.trRev)
        description: QbzSession.tr("Rename the My QBZ hub. Leave the name blank (or hit reset) to restore the default.", QbzSession.trRev)
        Row {
            spacing: 8
            QbzLineEdit { kioskHost: root.kioskHost;
                width: 150
                anchors.verticalCenter: parent.verticalCenter
                text: root.doc.myQbzLabel || ""
                placeholder: QbzSession.tr("My QBZ", QbzSession.trRev)
                onCommitted: function (v) { QbzBridge.settingsString("myqbz-label", v) }
            }
            SettingsButton { kioskHost: root.kioskHost;
                anchors.verticalCenter: parent.verticalCenter
                iconName: "rotate-ccw"
                onClicked: QbzBridge.settingsString("myqbz-label", "")
            }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Show collapsible My QBZ in sidebar", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.collapsibleMyqbz === true
            onToggled: function (v) { QbzBridge.settingsBool("collapsible-myqbz", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: root.doc.collapsibleMyqbz === true
        label: QbzSession.tr("Show Mixtapes", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.myqbzShowMixtapes === true
            onToggled: function (v) { QbzBridge.settingsBool("myqbz-show-mixtapes", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: root.doc.collapsibleMyqbz === true
        label: QbzSession.tr("Show Collections", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.myqbzShowCollections === true
            onToggled: function (v) { QbzBridge.settingsBool("myqbz-show-collections", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Invert swipe navigation direction", QbzSession.trRev)
        description: QbzSession.tr("Swap the two-finger touchpad swipe: left goes back, right goes forward.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.invertSwipeNavigation === true
            onToggled: function (v) { QbzBridge.settingsBool("invert-swipe-navigation", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Click on menu item navigates to first tab", QbzSession.trRev)
        description: QbzSession.tr("Clicking a section in the sidebar or title bar opens its first tab — including your chosen Local Library default — instead of only showing its menu", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.navClickFirstTab === true
            onToggled: function (v) { QbzBridge.settingsBool("nav-click-first-tab", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Show Purchases", QbzSession.trRev)
        // Purchases follows the other sections — sidebar or header — so the
        // separate "in title bar" placement toggle is gone (2026-09-13).
        description: QbzSession.tr("Show the Purchases section beside the other sections — in the sidebar or the header, wherever they are — for browsing and downloading your purchased music", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.showPurchases === true
            onToggled: function (v) { QbzBridge.settingsBool("show-purchases", v) }
        }
    }

    SettingsSpacer { }
    SettingsDivider { }
    SettingsSpacer { }

    // ============================== SEARCH ==============================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("SEARCH", QbzSession.trRev) }

    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Intelligent Search", QbzSession.trRev)
        description: QbzSession.tr("Smart search cache, ranking, and the search preview dropdown. On by default.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.intelligentSearch === true
            onToggled: function (v) { QbzBridge.settingsBool("intelligent-search", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Immersive search", QbzSession.trRev)
        description: QbzSession.tr("What selecting a result in the Immersive search does. Disabled turns the in-immersive search off.", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 200
            options: root.doc.immersiveSearchActions || []
            currentIndex: root.doc.immersiveSearchActionIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("immersive-search-action", i) }
        }
    }

    Item { width: 1; height: 40 }
}
