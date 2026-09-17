pragma Singleton
import QtQuick
QtObject {
    property var labels: []
    function openLabel(id) { labels = labels.concat([String(id)]) }
}
