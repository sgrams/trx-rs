import QtQuick 2.15
import QtQuick.Controls 2.15

ApplicationWindow {
    id: root
    visible: true
    width: 900
    height: 540
    title: "trx-rs"

    Column {
        anchors.centerIn: parent
        spacing: 10

        Label {
            text: "trx-rs Qt frontend (stub)"
            font.pixelSize: 20
        }

        Label { text: "Frequency: " + rig.freq_text + " (" + rig.freq_hz + " Hz)" }
        Label { text: "Mode: " + rig.mode + "  Band: " + rig.band }
        Label { text: "PTT: " + (rig.tx_enabled ? "TX" : "RX") + "  Power: " + (rig.powered ? "On" : "Off") }
        Label { text: "Lock: " + (rig.locked ? "Locked" : "Unlocked") }
        Label { text: "RX Sig: " + rig.rx_sig + " dB" }
        Label { text: "TX Pwr: " + rig.tx_power + "  Limit: " + rig.tx_limit + "  SWR: " + rig.tx_swr + "  ALC: " + rig.tx_alc }

        Row {
            spacing: 6

            TextField {
                id: freqInput
                width: 140
                placeholderText: "Freq (Hz)"
            }

            Button {
                text: "Set Freq"
                onClicked: rig.set_freq_hz(parseInt(freqInput.text))
            }

            TextField {
                id: modeInput
                width: 80
                placeholderText: "Mode"
            }

            Button {
                text: "Set Mode"
                onClicked: rig.set_mode(modeInput.text)
            }
        }

        Row {
            spacing: 6

            Button {
                text: rig.tx_enabled ? "PTT Off" : "PTT On"
                onClicked: rig.toggle_ptt()
            }
            Button {
                text: rig.powered ? "Power Off" : "Power On"
                onClicked: rig.toggle_power()
            }
            Button {
                text: "VFO"
                onClicked: rig.toggle_vfo()
            }
            Button {
                text: rig.locked ? "Unlock" : "Lock"
                onClicked: rig.locked ? rig.unlock_panel() : rig.lock_panel()
            }
        }

        Row {
            spacing: 6
            TextField {
                id: txLimitInput
                width: 120
                placeholderText: "TX Limit"
            }
            Button {
                text: "Set Limit"
                onClicked: rig.set_tx_limit(parseInt(txLimitInput.text))
            }
        }

        Rectangle {
            width: 540
            height: 120
            color: "#20252b"
            radius: 6

            Text {
                anchors.fill: parent
                anchors.margins: 8
                color: "#d0d6de"
                text: rig.vfo
                font.family: "monospace"
            }
        }
    }
}
