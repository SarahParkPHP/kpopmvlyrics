import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import com.kpopmvlyrics.kde

ApplicationWindow {
    id: window
    width: 980
    height: 680
    visible: true
    title: "K-Pop MV Lyrics"

    // ---- App state -------------------------------------------------------
    property var metadata: null
    property var songPackage: null
    property var alignment: []
    property var formats: []
    property string selectedFormat: "auto"
    property int currentMs: 0
    property bool syncRunning: false
    property string status: "Ready"
    property var languages: ({ original: true, romanization: false, english: true })

    Backend {
        id: backend
        Component.onCompleted: backend.initialize()
        onPosition: function(json) {
            const pos = JSON.parse(json);
            window.currentMs = pos.ms;
            window.syncRunning = pos.playing;
        }
        onPlayerError: function(message) { window.status = "Error: " + message }
    }

    // ---- Core helpers ----------------------------------------------------
    function call(command, args) {
        return JSON.parse(backend.invoke(command, JSON.stringify(args || {})));
    }

    function resolveVideo() {
        try {
            window.status = "Resolving video…";
            metadata = call("resolve_video_metadata", { url: urlField.text });
            formats = call("list_video_formats", { url: urlField.text });

            const spec = backend.player_load(JSON.stringify(call("resolve_stream", { url: urlField.text, formatId: null })));

            window.status = "Fetching lyrics…";
            const query = (metadata.title || metadata.originalUrl);
            songPackage = call("fetch_lyrics", { query: query });

            window.status = "Fetching captions…";
            const captions = call("fetch_captions", { videoId: metadata.videoId });

            if (songPackage.song.id && captions.length) {
                window.status = "Aligning…";
                alignment = call("align_lyrics", { songId: songPackage.song.id, videoId: metadata.videoId });
            }
            window.status = "Ready";
        } catch (err) {
            window.status = "Error: " + err;
        }
    }

    function activeIndex() {
        for (var i = 0; i < alignment.length; i++) {
            if (currentMs >= alignment[i].startMs && currentMs <= alignment[i].endMs)
                return alignment[i].lyricIndex;
        }
        return 0;
    }

    function fmt(ms) {
        ms = Math.max(0, ms);
        const m = Math.floor(ms / 60000);
        const s = Math.floor((ms % 60000) / 1000);
        return m + ":" + (s < 10 ? "0" : "") + s;
    }

    // ---- Layout ----------------------------------------------------------
    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 12
        spacing: 10

        // Video surface placeholder. Real rendering attaches a GStreamer sink
        // to a native window handle (see README — needs winId/qml6glsink wiring).
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 240
            color: "black"
            Text {
                anchors.centerIn: parent
                color: "#888"
                text: "Video surface — " + fmt(window.currentMs)
            }
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 6
            TextField {
                id: urlField
                Layout.fillWidth: true
                placeholderText: "Paste a YouTube MV URL"
                text: "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
            }
            ComboBox {
                id: qualityBox
                model: ["Auto"].concat(window.formats.map(function(f) { return f.label; }))
            }
            Button { text: "Open"; onClicked: window.resolveVideo() }
            Button {
                text: window.syncRunning ? "Pause" : "Play"
                onClicked: window.syncRunning ? backend.player_pause() : backend.player_play()
            }
            Button {
                text: "Reset"
                onClicked: { backend.player_pause(); backend.player_seek(0); window.currentMs = 0; }
            }
        }

        RowLayout {
            spacing: 8
            Label { text: "Variants:" }
            Repeater {
                model: ["original", "romanization", "english"]
                CheckBox {
                    text: modelData
                    checked: window.languages[modelData]
                    onToggled: { var l = window.languages; l[modelData] = checked; window.languages = l; }
                }
            }
            Item { Layout.fillWidth: true }
            Label { text: window.status }
        }

        ListView {
            id: lyricList
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            model: window.songPackage ? window.songPackage.lines : []
            delegate: Rectangle {
                width: ListView.view.width
                height: row.implicitHeight + 10
                color: modelData.index === window.activeIndex() ? "#22324a" : "transparent"
                radius: 6
                RowLayout {
                    id: row
                    anchors.fill: parent
                    anchors.margins: 6
                    spacing: 12
                    Label {
                        text: modelData.member || "All"
                        color: "#9aa"
                        Layout.preferredWidth: 90
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        Label {
                            visible: window.languages.original
                            text: modelData.original
                            wrapMode: Text.WordWrap
                            Layout.fillWidth: true
                        }
                        Label {
                            visible: window.languages.romanization && modelData.romanization
                            text: modelData.romanization || ""
                            color: "#9aa"
                            Layout.fillWidth: true
                        }
                        Label {
                            visible: window.languages.english && modelData.english
                            text: modelData.english || ""
                            color: "#9aa"
                            Layout.fillWidth: true
                        }
                    }
                }
            }
        }
    }
}
