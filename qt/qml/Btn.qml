import QtQuick
import QtQuick.Controls

// A text button that reads as unavailable when disabled (the Basic style keeps its colours).
Button {
    opacity: enabled ? 1 : 0.4
}
