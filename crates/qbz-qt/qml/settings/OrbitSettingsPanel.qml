// The gate is on construction, not merely visibility. Nothing in the Orbit
// panel is created in a normal launch or while another section is selected.
pragma ComponentBehavior: Bound
import QtQuick
import com.blitzfc.qbz
Loader {
    id: root
    property bool selected: false
    property bool kioskHost: false
    active: QbzOrbit.enabled && selected
    visible: active
    sourceComponent: Component {
        OrbitSettings { width: root.width; kioskHost: root.kioskHost }
    }
}
