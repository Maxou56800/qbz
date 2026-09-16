pragma Singleton
import QtQuick
QtObject {
    // Load the real repository SVGs; the executable normally serves generated
    // tints through this singleton. Tests exercise light/dark polarity.
    function dirFor(tint, json) {
        var light = json !== "" && JSON.parse(json).isDark === false
        var dir = tint
        if (tint === "textPrimary" || tint === "accentText") dir = light ? "black" : "primary"
        if (tint === "textSecondary" || tint === "secondary") dir = light ? "black" : "secondary"
        if (tint === "textMuted" || tint === "disabled" || tint === "textDisabled") dir = "muted"
        if (tint === "white") dir = "primary"
        return Qt.resolvedUrl("../../../../../../crates/qbz-qt/qml/assets/icons/" + dir).toString()
    }
}
