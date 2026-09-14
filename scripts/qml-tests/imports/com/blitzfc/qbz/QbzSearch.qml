pragma Singleton
import QtQuick
QtObject {
    property var submitted: []
    function searchSubmit(text) { submitted = submitted.concat([String(text)]) }
}
