// QbzClipboard — the one clipboard write QML offers without a C++ seam: an
// off-screen TextEdit's copy(). Mount one where a copy action lives and call
// `copy(text)`; the carrier is emptied right after, so the text never lingers
// in an invisible editor. One primitive instead of the carriers CommandBlock,
// SandboxSettings and the lyrics flyout each grew on their own.

import QtQuick

Item {
    id: root
    visible: false
    width: 0
    height: 0

    /// Put `text` on the system clipboard. False for an empty text.
    function copy(text) {
        var s = String(text || "")
        if (s === "")
            return false
        carrier.text = s
        carrier.selectAll()
        carrier.copy()
        carrier.deselect()
        carrier.text = ""
        return true
    }

    TextEdit {
        id: carrier
        visible: false
        width: 0
        height: 0
    }
}
