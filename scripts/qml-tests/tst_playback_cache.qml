import QtQuick
import QtTest
import com.blitzfc.qbz
import "../../crates/qbz-qt/qml/settings" as Settings

Item {
    width: 1000; height: 2800
    Settings.PlaybackSettings { id: settings; width: 950 }
    QtObject {
        id: confirmation
        property var pending: null
        property string body: ""
        function ask(title, text, label, cb) { body = text; pending = cb }
    }
    TestCase {
        name: "PlaybackCache"; when: windowShown
        function seed(dynamic, min, max, streamingOnly) {
            settings.doc = { streamingOnly: streamingOnly || false,
                playbackCache: {dynamic:dynamic, min_mib:min, max_mib:max},
                playbackCacheUsage: {recommended_size_bytes:419430400,
                    current_size_bytes:104857600, max_size_bytes:419430400,
                    ceiling_size_bytes:1677721600} }
            waitForRendering(settings)
        }
        function row(label) {
            var pending = [settings]
            while (pending.length) {
                const item = pending.pop()
                if (item.label === label && item.control !== undefined) return item
                if (item.children)
                    for (const child of item.children) pending.push(child)
            }
            fail("Missing settings row: " + label)
        }
        function control(label) {
            const names = {"Rebuild playback buffers":"playbackMemoryApply",
                "Recommended cache defaults":"playbackMemoryReset",
                "Playback storage folder":"playbackStoragePath"}
            return names[label] ? findChild(settings,names[label]) : row(label).control[0]
        }
        function profile(id) { return findChild(settings,"memoryProfile-" + id) }
        function init() {
            // Finish a previous field's focus-loss commit before clearing calls.
            settings.forceActiveFocus()
            QbzPlayer.npHasTrack = false
            QbzPlayer.npPlaying = false
            QbzPlayer.npLoading = false
            QbzPlayer.npTrackId = ""
            settings.confirmHost = confirmation
            confirmation.pending = null
            settings.width=950
            seed(false, null, null, false)
            QbzBridge.boolCalls = []
            QbzBridge.stringCalls = []
        }
        function test_streaming_only_keeps_gapless_available() {
            seed(false, null, null, true)
            const gapless = control("Gapless playback")
            verify(gapless.enabled)
            mouseClick(gapless)
            compare(QbzBridge.boolCalls.length, 1)
            compare(QbzBridge.boolCalls[0].key, "gapless")
            compare(QbzBridge.boolCalls[0].value, true)
        }
        function test_apply_idle_is_direct_and_busy_is_disabled() {
            mouseClick(control("Rebuild playback buffers"))
            compare(QbzBridge.stringCalls.length, 1)
            compare(QbzBridge.stringCalls[0].key, "playback-cache-apply")
            compare(QbzBridge.stringCalls[0].value, "idle:")
            compare(confirmation.pending, null)
            settings.doc = {playbackMemoryApplyBusy:true}
            verify(!control("Rebuild playback buffers").enabled)
        }
        function test_storage_selection_preserves_active_path_until_restart() {
            settings.doc = {playbackStorage:{candidate:"/mnt/external", active:"/old/qbz-playback",
                sandbox:"Flatpak",command:"flatpak override --user --filesystem='/mnt/external:rw' 'com.blitzfc.qbz'"}}
            wait(0)
            var field = control("Playback storage folder")
            compare(field.text, "/mnt/external")
            verify(field.width <= findChild(settings,"playbackStorageBrowse").width * 2)
            verify(findChild(settings,"playbackStorageActiveHint").text.indexOf("/old/qbz-playback") >= 0)
            mouseClick(field)
            keyClick(Qt.Key_A, Qt.ControlModifier)
            keyClick(Qt.Key_Backspace)
            keyClick(Qt.Key_Return)
            var change = QbzBridge.stringCalls[QbzBridge.stringCalls.length - 1]
            compare(change.key, "playback-storage-folder")
            compare(change.value, "")
            verify(findChild(settings,"playbackStorageActiveHint").text.indexOf("/old/qbz-playback") >= 0)
        }
        function test_apply_requires_confirmation_and_keeps_original_track_token() {
            QbzPlayer.npHasTrack = true
            QbzPlayer.npPlaying = true
            QbzPlayer.npTrackId = "163001663"
            mouseClick(control("Rebuild playback buffers"))
            compare(QbzBridge.stringCalls.length, 0)
            verify(confirmation.body.indexOf("0:00") >= 0)
            verify(confirmation.pending !== null)
            QbzPlayer.npTrackId = "163001664"
            confirmation.pending()
            compare(QbzBridge.stringCalls.length, 1)
            compare(QbzBridge.stringCalls[0].value, "restart:163001663")
            // Closing/cancelling the host drops the callback: no action.
            QbzBridge.stringCalls = []
            mouseClick(control("Rebuild playback buffers"))
            confirmation.pending = null
            compare(QbzBridge.stringCalls.length, 0)
        }
        function test_apply_paused_does_not_ask_or_request_play() {
            QbzPlayer.npHasTrack = true
            QbzPlayer.npTrackId = "163001663"
            mouseClick(control("Rebuild playback buffers"))
            compare(confirmation.pending, null)
            compare(QbzBridge.stringCalls.length, 1)
            compare(QbzBridge.stringCalls[0].value, "idle:163001663")
        }
        function test_memory_profiles_select_and_explain_every_preset() {
            verify(profile("auto").selected)
            for (var i=0; i<5; i++) {
                var option = profile(settings.memoryProfileIds[i])
                verify(option.visible)
                compare(option.description, settings.memoryProfileDescriptions[i])
                verify(option.description.length > 70)
                option.forceActiveFocus()
                keyClick(Qt.Key_Space)
                compare(QbzBridge.stringCalls[QbzBridge.stringCalls.length-1].key,"playback-memory-profile")
                compare(QbzBridge.stringCalls[QbzBridge.stringCalls.length-1].value,settings.memoryProfileIds[i])
                settings.doc={playbackMemoryProfile:settings.memoryProfileIds[i]}
                for (var j=0;j<5;j++) compare(profile(settings.memoryProfileIds[j]).selected,i===j)
            }
            settings.doc={playbackMemoryProfile:"high",playbackCache:{dynamic:true,min_mib:400,max_mib:1600}}
            verify(!findChild(settings,"playbackMemoryCustomOptions").visible)
            verify(profile("high").description.indexOf("1600 MiB")>=0)
            profile("high").forceActiveFocus()
            keyClick(Qt.Key_Down)
            compare(QbzBridge.stringCalls[QbzBridge.stringCalls.length-1].value,"desktop")
        }
        function test_legacy_edits_display_custom_and_streaming_only_disables_profiles() {
            seed(true,512,2048,false)
            verify(profile("custom").selected)
            verify(findChild(settings,"playbackMemoryCustomOptions").visible)
            seed(false,null,null,true)
            for (const id of settings.memoryProfileIds) verify(!profile(id).enabled)
            verify(findChild(settings,"playbackMemoryStreamingNotice").visible)
            seed(false,null,null,false)
            verify(!findChild(settings,"playbackMemoryStreamingNotice").visible)
        }
        function test_profile_explanations_fit_narrow_settings() {
            settings.width=550
            settings.doc={playbackMemoryProfile:"low"}
            wait(0)
            var group=findChild(settings,"playbackMemoryFieldset")
            for (var i=0;i<5;i++) {
                var radio=profile(settings.memoryProfileIds[i])
                verify(radio.height>35)
                verify(radio.width<=group.width)
                if (i>0) verify(radio.y>=profile(settings.memoryProfileIds[i-1]).y+profile(settings.memoryProfileIds[i-1]).height)
            }
            var actions=findChild(settings,"playbackMemoryActions")
            verify(actions.width<=group.contentItem.width)
            verify(actions.y>=findChild(settings,"playbackMemoryTradeoff").y+findChild(settings,"playbackMemoryTradeoff").height)
            verify(row("Memory cache usage").y>=group.y+group.height)
            compare(control("Recommended cache defaults").parent,control("Rebuild playback buffers").parent)
            const compactHeight=group.height
            settings.doc={playbackMemoryProfile:"custom"}
            tryVerify(function(){return group.height>compactHeight})
            verify(control("Minimum cache budget (MiB)").visible)
        }
        function test_recommended_defaults_and_usage() {
            verify(!control("Grow cache when memory is available").checked)
            compare(control("Minimum cache budget (MiB)").text, "")
            compare(control("Minimum cache budget (MiB)").placeholder, "400")
            verify(!control("Maximum cache budget (MiB)").enabled)
            verify(row("Memory cache usage").description.indexOf("100 / 400 MiB") >= 0)
        }
        function test_automatic_minimum_shows_the_resolved_host_budget() {
            settings.doc = {playbackCache:{dynamic:true, min_mib:null, max_mib:100},
                playbackCacheUsage:{recommended_size_bytes:419430400, base_size_bytes:104857600}}
            compare(control("Minimum cache budget (MiB)").placeholder, "100")
            settings.doc = {playbackCache:{dynamic:false, min_mib:null, max_mib:null},
                playbackCacheUsage:{recommended_size_bytes:52428800, base_size_bytes:52428800}}
            compare(control("Minimum cache budget (MiB)").placeholder, "50")
        }
        function test_growth_toggle_is_connected_and_enables_maximum() {
            seed(false,400,null,false)
            mouseClick(control("Grow cache when memory is available"))
            compare(QbzBridge.boolCalls[0].key, "playback-cache-dynamic")
            compare(QbzBridge.boolCalls[0].value, true)
            seed(true, null, null, false)
            verify(control("Maximum cache budget (MiB)").enabled)
        }
        function test_editing_both_limits_sends_mib_values() {
            seed(true, 400, 1600, false)
            var minimum = control("Minimum cache budget (MiB)")
            mouseClick(minimum)
            keyClick(Qt.Key_A, Qt.ControlModifier)
            keyClick(Qt.Key_5); keyClick(Qt.Key_1); keyClick(Qt.Key_2)
            keyClick(Qt.Key_Return)
            var change = QbzBridge.stringCalls[QbzBridge.stringCalls.length - 1]
            compare(change.key, "playback-cache-min")
            compare(change.value, "512")
            var maximum = control("Maximum cache budget (MiB)")
            mouseClick(maximum)
            keyClick(Qt.Key_A, Qt.ControlModifier)
            keyClick(Qt.Key_2); keyClick(Qt.Key_0); keyClick(Qt.Key_4); keyClick(Qt.Key_8)
            keyClick(Qt.Key_Return)
            change = QbzBridge.stringCalls[QbzBridge.stringCalls.length - 1]
            compare(change.key, "playback-cache-max")
            compare(change.value, "2048")
        }
        function test_restore_defaults_reseeds_controls() {
            seed(true, 512, 2048, false)
            mouseClick(control("Recommended cache defaults"))
            compare(QbzBridge.stringCalls[0].key, "playback-cache-reset")
            seed(false, null, null, false)
            compare(control("Minimum cache budget (MiB)").text, "")
            compare(control("Maximum cache budget (MiB)").text, "")
            verify(!control("Grow cache when memory is available").checked)
        }
        function test_streaming_only_disables_cache_mutations_but_allows_refresh() {
            seed(true, 400, 1600, true)
            verify(!control("Grow cache when memory is available").enabled)
            verify(!control("Minimum cache budget (MiB)").enabled)
            verify(!control("Maximum cache budget (MiB)").enabled)
            verify(!control("Rebuild playback buffers").enabled)
            verify(!control("Recommended cache defaults").enabled)
            mouseClick(control("Memory cache usage"))
            compare(QbzBridge.stringCalls[0].key, "playback-cache-refresh")
        }
    }
}
