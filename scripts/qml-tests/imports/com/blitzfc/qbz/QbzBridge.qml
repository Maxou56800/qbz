pragma Singleton
import QtQuick
QtObject {
    property var boolCalls: []
    property var stringCalls: []
    function settingsBool(key, value) { boolCalls = boolCalls.concat([{key:key, value:value}]) }
    function settingsString(key, value) { stringCalls = stringCalls.concat([{key:key, value:value}]) }
}
